//! One connection attempt as a tokio task: dial, TLS, line framing, an
//! ordered and paced write queue, and PONG. It holds no protocol state
//! beyond that; everything it hears goes to the model as a
//! `(ConnId, TransportEvent)`, ending with exactly one `Closed`.

use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::AbortHandle;
use tokio::time::{Instant, sleep_until, timeout};
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::client::danger::{
    HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
};
use tokio_rustls::rustls::crypto::{self, WebPkiSupportedAlgorithms};
use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use tokio_rustls::rustls::{self, ClientConfig, DigitallySignedStruct, SignatureScheme};

use textual_core::irc::{Message, WireLine};
use textual_core::model::network::{Address, CloseReason, ConnId, Tls, TransportEvent};

/// Longest line accepted from the server: 8191 bytes of tags plus a 512-byte
/// body, per the IRCv3 message-tags spec, plus the CRLF.
const MAX_LINE: usize = 8191 + 512 + 2;

pub type Events = mpsc::UnboundedSender<(ConnId, TransportEvent)>;

#[derive(Debug, Clone, Copy)]
pub struct Settings {
    /// Covers the TCP connect and the TLS handshake together.
    pub connect_timeout: Duration,
    pub pacing: Pacing,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            connect_timeout: Duration::from_secs(30),
            pacing: Pacing::default(),
        }
    }
}

/// At most `burst` queued lines go out in any `interval`. PONG is not
/// counted and never waits.
#[derive(Debug, Clone, Copy)]
pub struct Pacing {
    pub burst: usize,
    pub interval: Duration,
}

/// Textual's flood control defaults: 6 lines per 2 s.
impl Default for Pacing {
    fn default() -> Self {
        Pacing {
            burst: 6,
            interval: Duration::from_secs(2),
        }
    }
}

/// The app's handle on one attempt. Dropping it, or `hangup`, ends the task
/// at once, wherever it is (dialing, handshaking or talking), and no further
/// event is sent.
pub struct Connection {
    lines: mpsc::UnboundedSender<WireLine>,
    task: AbortHandle,
}

impl Connection {
    /// Queues a line. Lines go out in the order given; a line queued after
    /// the connection ended is dropped.
    pub fn send(&self, line: WireLine) {
        let _ = self.lines.send(line);
    }

    pub fn hangup(self) {}
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub fn spawn(
    runtime: &tokio::runtime::Handle,
    conn: ConnId,
    address: Address,
    tls: Tls,
    settings: Settings,
    events: Events,
) -> Connection {
    let (lines, queue) = mpsc::unbounded_channel();
    let task = runtime.spawn(async move {
        let reason = match timeout(settings.connect_timeout, open(&address, tls)).await {
            Err(_) => CloseReason::ConnectTimeout,
            Ok(Err(reason)) => reason,
            Ok(Ok(stream)) => {
                if events.send((conn, TransportEvent::Opened)).is_err() {
                    return;
                }
                talk(conn, stream, queue, settings.pacing, &events).await
            }
        };
        let _ = events.send((conn, TransportEvent::Closed(reason)));
    });
    Connection {
        lines,
        task: task.abort_handle(),
    }
}

trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<S: AsyncRead + AsyncWrite + Unpin + Send> Stream for S {}

async fn open(address: &Address, tls: Tls) -> Result<Box<dyn Stream>, CloseReason> {
    let tcp = TcpStream::connect((address.host.as_str(), address.port))
        .await
        .map_err(error)?;
    let _ = tcp.set_nodelay(true);
    let Tls::On { verify } = tls else {
        return Ok(Box::new(tcp));
    };
    let name = ServerName::try_from(address.host.clone())
        .map_err(|_| CloseReason::Error(format!("{:?} is not a host name", address.host)))?;
    let (config, rejected) = client_config(verify)?;
    match TlsConnector::from(Arc::new(config))
        .connect(name, tcp)
        .await
    {
        Ok(stream) => Ok(Box::new(stream)),
        Err(e) => Err(match rejected.lock().expect("not poisoned").take() {
            Some(sha256) => CloseReason::BadCertificate { sha256 },
            None => error(e),
        }),
    }
}

fn error(e: io::Error) -> CloseReason {
    CloseReason::Error(e.to_string())
}

type Rejected = Arc<Mutex<Option<[u8; 32]>>>;

fn client_config(verify: bool) -> Result<(ClientConfig, Rejected), CloseReason> {
    let provider = Arc::new(crypto::aws_lc_rs::default_provider());
    let inner: Arc<dyn ServerCertVerifier> = if verify {
        let platform = rustls_platform_verifier::Verifier::new(provider.clone())
            .map_err(|e| CloseReason::Error(e.to_string()))?;
        Arc::new(platform)
    } else {
        Arc::new(AcceptAny(provider.signature_verification_algorithms))
    };
    let rejected = Rejected::default();
    let verifier = Recording {
        inner,
        rejected: rejected.clone(),
    };
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| CloseReason::Error(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    Ok((config, rejected))
}

/// Wraps the real verifier to keep the fingerprint of a certificate it
/// rejects: the handshake error alone does not carry the certificate.
#[derive(Debug)]
struct Recording {
    inner: Arc<dyn ServerCertVerifier>,
    rejected: Rejected,
}

impl ServerCertVerifier for Recording {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let result = self.inner.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        );
        if let Err(rustls::Error::InvalidCertificate(_)) = result {
            let sha256 = Sha256::digest(end_entity.as_ref()).into();
            *self.rejected.lock().expect("not poisoned") = Some(sha256);
        }
        result
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

/// `verify = false`: any certificate is accepted, but the handshake is still
/// checked against it, so the session is at least bound to that key.
#[derive(Debug)]
struct AcceptAny(WebPkiSupportedAlgorithms);

impl ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        crypto::verify_tls12_signature(message, cert, dss, &self.0)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        crypto::verify_tls13_signature(message, cert, dss, &self.0)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_schemes()
    }
}

async fn talk(
    conn: ConnId,
    stream: Box<dyn Stream>,
    mut incoming: mpsc::UnboundedReceiver<WireLine>,
    pacing: Pacing,
    events: &Events,
) -> CloseReason {
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = LineReader::new(reader);
    let mut queue = VecDeque::new();
    let mut window = Window::new(pacing);
    loop {
        let send_at = (!queue.is_empty()).then(|| window.next_slot());
        let step: io::Result<Option<CloseReason>> = tokio::select! {
            line = reader.next_line() => match line {
                Ok(Some(text)) => {
                    let Ok(msg) = Message::parse(&text) else { continue };
                    if msg.command == "PING" {
                        let pong = Message {
                            command: "PONG".into(),
                            params: msg.params.clone(),
                            ..Default::default()
                        };
                        if let Ok(pong) = pong.to_wire()
                            && let Err(e) = writer.write_all(pong.as_bytes()).await
                        {
                            return error(e);
                        }
                    }
                    if events.send((conn, TransportEvent::Line(msg))).is_err() {
                        return CloseReason::Error("the app went away".into());
                    }
                    Ok(None)
                }
                Ok(None) => Ok(Some(CloseReason::Eof)),
                Err(e) => Err(e),
            },
            Some(line) = incoming.recv() => {
                // QUIT skips the paced queue: the model hangs up after its
                // grace period, and a QUIT still queued then is never sent.
                if line.as_str().starts_with("QUIT") {
                    writer.write_all(line.as_bytes()).await.map(|()| None)
                } else {
                    queue.push_back(line);
                    Ok(None)
                }
            }
            () = sleep_until(send_at.unwrap_or_else(Instant::now)), if send_at.is_some() => {
                let line = queue.pop_front().expect("a slot is only awaited for a queued line");
                window.record(Instant::now());
                writer.write_all(line.as_bytes()).await.map(|()| None)
            }
        };
        match step {
            Ok(None) => {}
            Ok(Some(reason)) => return reason,
            Err(e) => return error(e),
        }
    }
}

/// When each of the last `burst` paced lines went out.
struct Window {
    pacing: Pacing,
    sent: VecDeque<Instant>,
}

impl Window {
    fn new(pacing: Pacing) -> Self {
        Window {
            pacing,
            sent: VecDeque::with_capacity(pacing.burst),
        }
    }

    fn next_slot(&self) -> Instant {
        match self.sent.front() {
            Some(oldest) if self.sent.len() >= self.pacing.burst => *oldest + self.pacing.interval,
            _ => Instant::now(),
        }
    }

    fn record(&mut self, at: Instant) {
        if self.sent.len() >= self.pacing.burst {
            self.sent.pop_front();
        }
        self.sent.push_back(at);
    }
}

/// Splits a byte stream into lines. Safe to cancel between calls (as
/// `select!` does): a half-read line stays in `buf` for the next call.
struct LineReader<R> {
    inner: BufReader<R>,
    buf: Vec<u8>,
    overflowed: bool,
}

impl<R: AsyncRead + Unpin> LineReader<R> {
    fn new(inner: R) -> Self {
        LineReader {
            inner: BufReader::new(inner),
            buf: Vec::with_capacity(1024),
            overflowed: false,
        }
    }

    /// The next complete line without its terminator, or `None` at EOF.
    /// Lines longer than `MAX_LINE`, and a final line with no terminator,
    /// are dropped rather than passed on truncated.
    async fn next_line(&mut self) -> io::Result<Option<String>> {
        loop {
            let available = self.inner.fill_buf().await?;
            if available.is_empty() {
                return Ok(None);
            }
            let (chunk, done) = match available.iter().position(|&b| b == b'\n') {
                Some(i) => (&available[..=i], true),
                None => (available, false),
            };
            if self.buf.len() + chunk.len() > MAX_LINE {
                self.overflowed = true;
                self.buf.clear();
            } else if !self.overflowed {
                self.buf.extend_from_slice(chunk);
            }
            let consumed = chunk.len();
            self.inner.consume(consumed);
            if !done {
                continue;
            }
            let overflowed = std::mem::take(&mut self.overflowed);
            let mut line = std::mem::take(&mut self.buf);
            if overflowed {
                continue;
            }
            while matches!(line.last(), Some(b'\n' | b'\r')) {
                line.pop();
            }
            return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
        }
    }
}
