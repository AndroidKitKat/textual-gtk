# Rubric (picker and judge only; candidates never see this)

1. Single ownership: every piece of session state (nick, caps, ISUPPORT, channels, members, topic, unread) has exactly one owner and one writer; no copies that must be kept in sync between threads.
2. Testable core: the protocol/state logic is a pure or near-pure unit that runs in `cargo test` without GTK, sockets or timers, fed by recorded lines; the GTK side is a thin projection.
3. Correctness where Textual was wrong: CAP LS 302 multiline + batched REQ, SASL failure policy, reconnect with backoff, CASEMAPPING-correct identity, NAMES/WHO handling, echo-message.
4. UI projection fits GTK4: sidebar/member list as ListModels (or equivalent) updated incrementally from state changes, focused-item semantics, unread/highlight after render, stable CSS class names for the Mac stylesheet.
5. Interface depth and reader load: small public surfaces hiding real policy; short call chains (socket line to pixel traceable in <= 3 files); no hand-synced lists.
6. Phase fit: covers config+secrets persistence and input (history, completion, splitting) concretely enough to implement next, without phase-3 scope creep.
