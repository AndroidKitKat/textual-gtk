# Textual IRC client model: objects, connection state machine, dispatch

All paths are relative to `~/workspace/Textual-4f09312/Sources/App/Classes/`. "C" means `IRC/IRCClient.m`, which is 13,278 lines.

### Components Found

| Type | File | What it does |
|---|---|---|
| IRCWorld | `IRC/IRCWorld.m`, `Headers/IRCWorld.h` | Singleton (`worldController()`). Holds `NSMutableArray<IRCClient*> clients` (IRCWorld.m:62), builds clients/channels/queries (factory, :395-480), handles persistence (`save` → `TPCPreferences setClientList`, :114; `savePeriodically`, 300 s threshold, :119), sleep/wake/reachability fan-out (:200-260), and id lookup across all clients (`findItemWithId`, :330). |
| IRCTreeItem | `Headers/IRCTreeItem.h`, `Headers/Private/IRCTreeItemPrivate.h:46-47` | Common base of client and channel. Holds `uniqueIdentifier`, unread counters, `associatedClient` (**weak**) and a strong `viewController` (TVCLogController). |
| IRCClient | C, `Headers/IRCClient.h`, `Headers/Private/IRCClientPrivate.h` | One per configured network. It is the IRCConnection delegate and does everything else: state flags, CAP/SASL, the receive switch, the user table, the channel list, timers, autojoin, ZNC handling. |
| IRCClientConfig | `IRC/IRCClientConfig.m`, `Headers/IRCClientConfig.h:47-125` | Immutable/mutable value dict. Holds `uniqueIdentifier` (UUID, IRCClientConfig.m:123), `serverList [IRCServer]`, `channelList [IRCChannelConfig]`, nick/alt nicks/username/realName, `nicknamePassword` (keychain `textual.nickserv.<id>`, :837), proxy, flood control, `autoReconnect`, `autojoinWaitsForNickServ`, `saslAuthenticationDisableExternalMechanism`, the `znc*` flags, `lastMessageServerTime`, `identityClientSideCertificate`, `cipherSuites`. |
| IRCServer | `IRC/IRCServer.m`, `Headers/IRCServer.h` | One entry in the server list: address, port, `prefersSecuredConnection`, password (keychain `textual.server.<serverId>`, IRCServer.m:168), and its own UUID (:92). |
| IRCChannel | `IRC/IRCChannel.m`, `Headers/IRCChannel.h` | Channel, query or utility window. Holds `config`, `status` (Parted/Joining/Joined/Terminated, IRCChannel.h:45), `topic`, `modeInfo` (IRCChannelMode), `memberInfo` (IRCChannelMemberList), and the flags `channelNamesReceived`, `channelModesReceived`, `sentInitialWhoRequest`, `errorOnLastJoinAttempt` (IRCChannelPrivate.h). |
| IRCChannelConfig | `IRC/IRCChannelConfig.m`, `Headers/IRCChannelConfig.h:42-66` | `type` (Channel/PrivateMessage/Utility), `channelName`, `uniqueIdentifier` (UUID, :76), `autoJoin`, `defaultModes`/`defaultTopic`, `secretKey` (keychain `textual.cjoinkey.<id>`, :307), notification settings. |
| IRCChannelMemberList | `IRC/IRCChannelMemberList.m` | A channel's member list: a sorted `NSMutableArray<IRCChannelUser*>`. Every mutation goes through one global serial queue (:126-165) and is mirrored to the UI controller on the main thread. |
| IRCChannelUser | `IRC/Users/IRCChannelUser.m`, `Headers/IRCChannelUser.h` | Membership record: strong `user`, `modes` string (sorted highest rank first), conversation weights for tab completion, rank helpers. |
| IRCUser | `IRC/Users/IRCUser.m`, `Headers/IRCUser.h` | Immutable value object: nick, user, host, realName, isAway, isIRCop. Copies share one `persistentStore` (relations, remove timer) (IRCUser.m:221-238). |
| IRCUserRelations | `IRC/Users/IRCUserRelations.m` | `NSMutableDictionary<IRCChannel*, IRCChannelUser*>` for each user. It is the reverse index from user to memberships. |
| IRCISupportInfo | `IRC/IRCISupportInfo.m`, `Headers/IRCISupportInfo.h` | Parses 005 tokens and answers prefix/mode questions. It also parses mode strings (`parseModes`). |
| IRCChannelMode / IRCModeInfo | `IRC/IRCChannelMode.m`, `IRC/IRCModeInfo.m` | Channel modes (not member modes), stored as a dict from symbol to IRCModeInfo. |
| IRCConnection | `IRC/IRCConnection.m`, `Headers/IRCConnection.h` | Front end in the app process for a socket that lives in an XPC service (`com.codeux.app-utilities.Textual-IRCConnectionHost`, IRCConnection.m:133). It hops callbacks to the main thread. |
| IRCMessage / IRCPrefix | `IRC/IRCMessage.m`, `Headers/IRCPrefix.h` | Line parser: tags, prefix, command, params. Also sets `receivedAt`/`isHistoric` from server-time, and `batchToken`/`parentBatchMessage`. |
| IRCMessageBatch | `IRC/IRCMessageBatch.m` | Queue of open BATCHes and the messages buffered inside them. |
| IRCClientRequestedCommands | `IRC/IRCClientRequestedCommands.m` | Tracks hidden versus visible WHO/ISON requests so automated replies are not printed. |
| ZNC buffextras plugin | `../../Plugins/ZNC Additions/Classes/TPI_ZNCAdditions.m:230-360` | Turns `*buffextras` PRIVMSGs into synthetic JOIN/PART/QUIT/KICK/MODE/NICK messages with `isPrintOnlyMessage = YES`. |

### Ownership graph and identifiers

- **IRCWorld** owns its clients strongly (`clients`, IRCWorld.m:62).
- **IRCClient** strongly owns:
  - `config`
  - `channelListPrivate` (an ordered array; channels are inserted ahead of the first query, C:1194-1225)
  - `userListPrivate` (an `NSMutableDictionary` keyed by `nickname.lowercaseString`, C:249, C:2464-2473, C:2507-2524)
  - `supportInfo`, `socket`, `batchMessages`, the timers, and `trackedUsers` (C:184-259)
- **IRCChannel → client** is weak (`associatedClient`). The channel strongly owns `memberInfo` and `modeInfo`. Both are created in `activate` and set to nil in `resetStatus` (IRCChannel.m, `activate`/`resetStatus`).
- **IRCChannelUser → IRCUser** is strong. **IRCUser → persistentStore → relations → IRCChannelUser** is also strong. That is a deliberate retain cycle. It is broken by `disassociateWithChannel` on remove, `clearMembers`, or `resetStatus`.
- **IRCUser → client** is weak.
- Identifiers:
  - client = `config.uniqueIdentifier` (C:827)
  - channel = `config.uniqueIdentifier`
  - server = its own UUID
  - Channels are looked up by name with `isEqualToStringIgnoringCase` (C:2374-2392).
  - Users are looked up by `lowercaseString` key.
  - Tree-wide lookup by id is IRCWorld.m:330.
- **IRCUser copy-on-write:** to change a user you call `mutableCopy`, edit, then `addUserAndReturn:` (C:2507). That stores an immutable copy and calls `becamePrimaryUser` (IRCUser.m:313), which repoints every related IRCChannelUser at the new copy via `relinkRelations`. A rename removes the old key and adds the new one (`modifyUser:withBlock:`, C:2591-2605).
- **User garbage collection:** when a user's relations count reaches 0, a 5-minute GCD timer on a global queue calls `[client removeUser:]` (IRCUser.m:46, :252-300).

### Flow

**1. Startup.** `IRCWorld setupConfiguration` (IRCWorld.m:85) reads `[TPCPreferences clientList]` dicts and builds each IRCClientConfig. `createClientWithConfig:reload:` (:395) then:
- calls `[[IRCClient alloc] initWithConfig:]` (C:283), which calls `writePasswordsToKeychain` and `prepareInitialState` (C:300). That creates supportInfo and the timer objects: autojoin, ison, reconnect, retry, pong, who.
- creates an IRCChannel for each `config.channelList` entry (`createChannelWithConfig:...`, :428).
- adds the client to the world.

Auto-connect: `autoConnectAfterWakeup:` (:177) staggers clients 1 s apart, plus 8 s after wake. It calls `autoConnectWithDelay:afterWakeUp:` (C:11447).

**2. connect.** `connect:bypassProxy:` (C:11284):
1. Guards against isConnecting, isConnected, isQuitting, isDisconnecting, and system sleep.
2. Picks a server: a temporary redirect override if one is set, otherwise round-robin `lastServerSelected+1` through `config.serverList` (C:11335-11357).
3. Sets `connectType`, resets `disconnectType`, sets `isConnecting=YES` and `reconnectEnabled=YES`.
4. Builds an `IRCConnectionConfigMutable`: address, port, TLS flag, `validateServerCertificateChain`, client cert, cipher suites, proxy, flood control.
5. Calls `[[IRCConnection alloc] initWithConfig:onClient:]` and then `open` (C:11438). `open` starts XPC and sends `openWithConfig` (IRCConnection.m:209).

**3. TCP/TLS up.** The XPC callback `ircConnectionDidConnectToHost` (IRCConnection.m:452) hops synchronously to main, then calls `ircConnectionDidConnect:` (C:5804), which:
1. Starts the retry timer (240 s registration watchdog).
2. Sets `isConnecting=NO`, `isConnected=YES`, `userNickname = config.nickname`.
3. Sends `CAP LS 302`, then `PASS` (if the server has a password), `NICK`, then `USER <username> <0|8> * <realName>` (C:5854-5862).

TLS info arrives through `ircConnectionDidSecureConnection` (C:5781). That callback only prints; `isSecured` lives on the socket.

**4. Receive path.** XPC `ircConnectionDidReceiveData` (IRCConnection.m:504) runs **off main**. It decodes the bytes and calls `ircConnection:didReceiveData:` (C:5907), which:
1. Updates `lastMessageReceived` and bandwidth counters.
2. Parses `[[IRCMessage alloc] initWithLine:onClient:]` (parser at IRCMessage.m:195). This reads client capabilities to decide on server-time and batch handling.
3. Passes the message through `THOPluginDispatcher interceptServerInput` (the ZNC buffextras rewrite happens here).
4. Calls `filterBatchCommandIncomingData` (C:8081). Messages tagged with an open batch are queued and stop here.
5. Calls `processIncomingMessage` (C:5978), which runs `_processIncomingMessage` (C:5987) **synchronously on the main queue**.

**5. Dispatch switch** (C:5987-6114). It first runs `processIncomingMessageAttributes` (C:5950), which updates `lastMessageServerTime` and, under the playback cap, may un-mark a message as historic. Then:
- If `commandNumeric > 0`, it calls `receiveNumericReply` (C:8840). Numerics 401–596 (except 422) go to `receiveErrorNumericReply` (C:8846, C:10377).
- Otherwise it switches on `IRCCommandIndex indexOfRemoteCommand`:

| Command | Handler | What it changes |
|---|---|---|
| NOTICE / PRIVMSG | `receivePrivmsgAndNotice` (C:6244) | Text, CTCP, and the NickServ/ChanServ special cases. |
| ERROR | `receiveError` (C:7884) | On Excess Flood or SendQ it sets a disconnect callback that cancels reconnect. |
| INVITE | `receiveInvite` (C:7852) | — |
| JOIN | `receiveJoin` (C:7115) | Adds a membership. |
| KICK | `receiveKick` (C:7314) | Removes a member; if it is us, deactivates the channel and may rejoin after 3 s. |
| KILL | `receiveKill` (C:7541) | Removes the nick from every channel. |
| MODE | `receiveMode` (C:7749) | Channel and member modes. |
| NICK | `receiveNick` (C:7554) | Renames user and query. |
| PART | `receivePart` (C:7233) | Removes a member, or deactivates the channel if it is us. |
| PING | `receivePing` (C:8725) | Sends PONG. |
| QUIT | `receiveQuit` (C:7395) | Removes the member from every channel and deactivates the matching query. |
| TOPIC | `receiveTopic` (C:7817) | — |
| WALLOPS | `receiveWallops` (C:6218) | — |
| CAP / AUTHENTICATE | `updateConnectedToZNCPropertyWithMessage` (C:2289), then `receiveCapabilityOrAuthenticationRequest` (C:8564) | ZNC detection, then CAP/SASL. |
| AWAY | `receiveAwayNotifyCapability` (C:8738) | `modifyUserWithNickname:asAway:`. |
| BATCH | `receiveBatch` (C:7933) | — |
| CERTINFO | `receiveCertInfo` (C:7903) | ZNC tlsinfo only. |
| CHGHOST | `receiveChangeHost` (C:8043) | Modifies the user's username and host. |

Every path ends with `processBundlesServerMessage` (plugins).

**6. CAP negotiation** (C:8128-8720):
- **LS / NEW:** each token goes to `processPendingCapability`. It splits `name=a,b` (C:8526). Supported caps go into the `capabilitiesPending` array (C:8545). `sasl` goes to `processPendingCapabilityForSASL` (C:8625), which picks EXTERNAL if the socket has a client cert and EXTERNAL isn't disabled and is offered, else PLAIN if `nicknamePassword` is set. It adds `SASLGeneric` plus the chosen mechanism to pending.
- **ACK:** `toggleCapability:enabled:YES` (C:8482). For `sasl` it calls `sendSASLIdentificationRequest`, which sends `AUTHENTICATE PLAIN|EXTERNAL`, adds `IsInSASLNegotiation` to pending, and calls `pauseCapabilityNegotiation` (C:8687, C:8436). Other caps set a bit in `capabilities`. The ZNC server-time variants are folded into ServerTime, and plan.io/ZNC playback into Playback (C:8506-8516).
- **NAK / DEL:** the bit is cleared.
- After any CAP subcommand, `sendNextCapability` runs (C:8381). If not paused, it REQs the **first pending cap, one per line**, or sends `CAP END` when none are left.
- **AUTHENTICATE +:** `sendSASLIdentificationInformation` (C:8658). PLAIN sends base64 of `username\0username\0nicknamePassword` in 400-byte chunks, with a trailing `+` if needed. EXTERNAL sends `+`.
- **Numerics:**
  - 900 RPL_LOGGEDIN sets the `IsIdentifiedWithSASL` bit (C:10291).
  - 901 calls `resetSASLNegotiation` (C:10308).
  - 903/902/904/905/906/907/908 print the reply, clear `IsInSASLNegotiation`, and call `resumeCapabilityNegotiation`, which leads to CAP END (C:10325-10345).
  - A SASL failure does **not** abort the connection.

**7. Capabilities negotiated (complete list).**

| Category | Values |
|---|---|
| String list (`isCapabilitySupported`, C:8448) | away-notify, batch, chghost, echo-message (only if the `enableEchoMessageCapability` preference is on), identify-ctcp, identify-msg, multi-prefix, sasl, server-time, userhost-in-names, plan.io/playback, znc.in/playback, znc.in/self-message, znc.in/server-time, znc.in/server-time-iso, znc.in/tlsinfo |
| Public bits (IRCClient.h:65-82) | AwayNotify, Batch, EchoMessage, IdentifyCTCP, IdentifyMsg, IsIdentifiedWithSASL, IsInSASLNegotiation, MonitorCommand, MultiPrefix, Playback, ServerTime, UserhostInNames, WatchCommand, ZNCCertInfoModule, ZNCSelfMessage, ChangeHost |
| Private bits (IRCClientPrivate.h:46-54) | SASLGeneric, SASLPlainText, SASLExternal, ZNCServerTime, ZNCServerTimeISO, ZNCPlaybackModule, PlanioPlayback |
| Set from ISUPPORT, not CAP (IRCISupportInfo.m:250-290) | MONITOR (skipped on *.freenode.net), WATCH, NAMESX (sends `PROTOCTL NAMESX`, sets MultiPrefix), UHNAMES (sends `PROTOCTL UHNAMES`, sets UserhostInNames) |

**8. Registration (001)** goes through `receiveInit` (C:8753):
1. Starts the pong timer and stops the retry timer.
2. Sets `isLoggedIn=YES`, `supportInfo.serverAddress = sender`, `userNickname = param0`, `successfulConnects++`.
3. Calls `socket enforceFloodControl`.
4. Runs `config.loginCommands`.
5. If the ZNC tlsinfo cap is on, sends `ZNC *tlsinfo send-data`.
6. Calls `requestPlayback` (C:2193).
7. Activates existing queries.
8. Autojoin decision:
   - If `!autojoinWaitsForNickServ` or SASL succeeded, it calls `performAutoJoin` right away.
   - Otherwise, on ZNC it calls `performAutoJoin` after 3 s.
   - Otherwise it starts the 90 s delayed-warning timer, and autojoin fires when the NickServ "identified" token is seen (C:6795-6806).
9. Calls `populateISONTrackedUsersList` 10 s later, which starts the ISON (30 s) and WHO (120 s) timers.

`performAutoJoin` (C:10744) shuffles the `autoJoin` channels and joins `autojoinMaximumChannelJoins` of them per `autojoinDelayBetweenChannelJoins` tick (C:10695). Keyed and unkeyed channels go in separate JOIN lines (C:11729-11786). Each channel's status is set to Joining.

Nick collisions before login (433/432/437) go to `receiveNicknameCollisionError` (C:10539). It tries `alternateNicknames[n]`, then pads with `_` to the default max length, falling back to `"0"` (C:10566).

**9. ISUPPORT** goes to `RPL_ISUPPORT` (C:8885), which joins the params minus the nick and trailing text, then calls `processConfigurationData` (IRCISupportInfo.m:117). Tokens are merged additively. Each 005 line is cached as its own dict.
- **PREFIX** goes to `parseUserModeSymbols` (:340). It stores two **ordered** arrays, `modeSymbols` and `characters`, and marks each prefix mode with value 100 in `channelModes` so it takes a parameter. Rank = 100 − index (:478).
- **CHANMODES** goes to `parseChannelModes` (:410). It maps each mode to its group 1–4. `modeHasParameter` (:385): groups 1, 2 and prefix modes always take a parameter; group 3 only when set.
- **CHANTYPES** sets `channelNamePrefixes`, used by `isChannelNameOn` (NSStringHelper.m:208, which also treats `~#` as a ZNC partyline).
- **NETWORK** sets `networkName` and `networkNameFormatted`, used for `IRCClient.networkName` (C:837).
- Also parsed: NICKLEN (used by `isHostmaskNicknameOn`, NSStringHelper.m:170), STATUSMSG, MODES, AWAYLEN, KICKLEN, TOPICLEN, KEYLEN, CHANNELLEN, EXCEPTS, INVEX.
- **CASEMAPPING is not parsed anywhere.** A grep across the whole tree for `casemapping` or `rfc1459` returns nothing.
- Defaults come from `reset` (:92): `#`, `(ov)@+`, `o`/`v` taking parameters. `reset` runs on every disconnect (C:5672).

**10. Membership tracking:**
- **Self JOIN** (C:7129):
  1. `findChannelOrCreate`.
  2. If the channel is not active, `activate` (`resetStatus:Joined`, a new member list and IRCChannelMode). If it is already active, the handler **returns** without further processing.
  3. Sets `userHostmask`.
  4. Then, for both self and others, takes a mutable copy of the user, sets nick/user/host, calls `addUserAndReturn`, then `channel addMember:checkForDuplicates:YES` (C:7152-7163).
  5. A self JOIN also sends `MODE #chan` (`requestModesForChannel`).
- **NAMES** (353, C:9693): ignored once `channelNamesReceived` is set. For each token it strips the prefix characters into mode letters (`modeSymbolForUserPrefix`), splits `nick!user@host` (uhnames), creates the IRCUser if missing (existing users are not updated), and adds an IRCChannelUser with those modes. **366** (C:9797) sets `channelNamesReceived`. If we are the only member, it applies `defaultModes`/`defaultTopic`.
- **WHO** (352, C:9531): skipped if a visible WHO is pending. It parses `<H|G>[*][prefixes]`: G sets away (only if away is monitored), `*` sets IRCop, prefix characters become modes. It updates the IRCUser (user/host/realName/away/ircop), adds the member if missing, resorts on an IRCop change, and caches `userHostmask` for self.
  - The periodic WHO scheduler (C:13004) sends at most 4 channels and about 2000 members per tick.
  - After the first pass, it skips channels when away-notify is on or when the channel is larger than `trackUserAwayStatusMaximumChannelSize`.
- **PART/KICK:** self deactivates the channel (`resetStatus:Parted` calls `clearMembers` and sets memberInfo/modeInfo/topic to nil). Others: `removeMemberWithNickname`.
- **QUIT:** for every channel, removes the member found via `user userAssociatedWithChannel`; a matching query is deactivated. The IRCUser itself is not removed. It gets GC'd 5 minutes after its relations reach 0.
- **NICK** (C:7554):
  1. For self: updates `userNickname` and `tryingNicknameSentNickname`.
  2. Posts `IRCClientUserNicknameChangedNotification`.
  3. `renameUser` re-keys the user dict.
  4. Every channel resorts the member, and a query with the old name is renamed unless one with the new name exists.
- **MODE** (C:7749): a non-channel target is printed as a UMODE (no state change, and `userIsIRCop` is not updated from it). For a channel, `modeInfo updateModes` parses through ISUPPORT and stores the modes. Each `IRCModeInfo` whose symbol is a PREFIX mode and has a parameter goes to `channel changeMember:mode:value:` (IRCChannelMemberList.m:470). That re-inserts the mode letter at its rank position in the member's `modes` string, treats `+Y` as IRCop, and replaces and resorts the member.
- **324 RPL_CHANNELMODEIS** (C:9300) does `modeInfo clear` then `updateModes`.
- **332/333** update the topic.
- Member order: rank-desc, then nickname with `caseInsensitiveCompare` (IRCChannelUser.m:275-301). Members are inserted by binary search (IRCChannelMemberList.m:173).

**11. Disconnect and reconnect:**
- **`quit`** (C:11525/11538): sets `isQuitting`, `cancelReconnect`, clears the send queue, sends `QUIT :comment`, and forces `disconnect` after 2 s. If not logged in, it disconnects right away.
- **`disconnect`** (C:11506): sets `isDisconnecting`, then `socket close`.
- **`ircConnection:didDisconnectWithError:`** (C:5865) runs `changeStateOffWithError:` (C:5644), which:
  1. Clears the socket and stops all timers.
  2. Starts the reconnect timer **if `reconnectEnabled`**.
  3. Resets supportInfo, the address-book cache, and tracked users.
  4. Maps the disconnect reason to a message. Reasons are Normal / ComputerSleep / BadCertificate (derived from `IRCConnectionErrorCodeBadCertificate`) / ReachabilityChange / ServerRedirect (IRCClient.h:57-63, C:5690-5735).
  5. Deactivates every active channel.
  6. Calls `resetAllPropertyValues` (C:5572), which empties the user dict, the caps, and `capabilitiesPending`, and resets `reconnectEnabled=NO`.
  7. Then runs `disconnectCallback` if one is set.
- **Reconnect timer:** fixed **20 s, repeating, no backoff** (C:155-161, C:10905). It is gated by `autoReconnect`, or by `autoSleepModeDisconnect` for sleep reconnects. Each tick calls `connect:Reconnect`, which advances to the next server.
- **Retry timer:** 240 s after TCP connect. If 001 has not arrived, it disconnects and calls `connect:Retry` (C:10956).
- **Pong timer** (every 30 s, C:10860):
  - After 270 s idle it sends `PING <serverAddress>` (if `performPongTimer`).
  - After 360 s idle it disconnects if EOF was received or `performDisconnectOnPongTimer` is set; otherwise it warns once.
  - PONG bypasses flood control (IRCConnection.m:427).
- **005 RPL_REDIR** (C:8910): sets ServerRedirect, sets a temporary address/port override, and reconnects via `disconnectCallback`.
- **Reachability loss** (C:1161): sets ReachabilityChange, `reconnectEnabled=YES`, then `disconnect`.
- **Sleep:** IRCWorld `prepareForSleep` (IRCWorld.m:205) sets ComputerSleep and calls `quit`; wake calls `autoConnectAfterWakeUp`, which uses Reconnect mode.

**12. ZNC and playback:**
- **ZNC detection** (C:2289): a server-sourced CAP/AUTHENTICATE line whose sender is `irc.znc.in` sets `isConnectedToZNC`. This is the only detection path.
- **`requestPlayback`** (C:2193), when the Playback cap is on:
  - Sends `play * <lastMessageServerTime>` if `successfulConnects>1`, or on the first connect when `zncOnlyPlaybackLatest` is set and a stored time exists. Otherwise it sends `play * 0`.
  - On ZNC it goes out as `ZNC *playback ...`; otherwise as `PRIVMSG *playback`.
  - `lastMessageServerTime` is persisted into the config (C:597, C:917) and saved periodically.
- **`znc.in/playback` BATCH** toggles `zncBouncerIsPlayingBackHistory` (C:8020, C:8060). `isSafeToPostNotificationForMessage` (C:2262) suppresses notifications for playback batches or historic messages when `zncIgnorePlaybackNotifications` is set.
- **ZNC PART suppression:** ZNC's fake PARTs while quitting are dropped (C:7240).
- **Queries:** `*module` queries are recognized (`nicknameIsZNCUser`, C:2232).
- **Self-message handling:** on ZNC, or with echo-message or self-message enabled, a PRIVMSG from our own nick is routed to the target's query (C:6534-6550).
- **Autojoin** is skipped on ZNC if `zncIgnoreConfiguredAutojoin` is set.

**13. Casemapping comparisons.** Everything uses Unicode case folding:
- `nicknameIsMyself` uses `isEqualToStringIgnoringCase` (C:984).
- User keys use `lowercaseString` (C:2468, IRCUser.m:179).
- Channel lookup uses `isEqualToStringIgnoringCase` (C:2383).
- Query matching on QUIT/NICK uses `isEqualToStringIgnoringCase` (C:7459, C:7672).

No RFC1459 `[]\~` ↔ `{}|^` folding exists anywhere.

### Threading and queue ownership

- **XPC side:** the socket and line framing live in the XPC helper. Connect/TLS/close/disconnect callbacks are forwarded synchronously to main (IRCConnection.m:445-530).
- **Received data:** `didReceiveData` decoding, IRCMessage parsing, the plugin intercept, and batch queueing all run on the XPC reply queue (C:5904-5946). After that, `processIncomingMessage` does `XRPerformBlockSynchronouslyOnMainQueue` (C:5982). All handlers run on **main**.
- **Member lists:** every member list in the app shares one global serial queue, `IRCChannel.modifyMemberListSerialQueue` (IRCChannelMemberList.m:126), accessed with `dispatch_sync` plus a re-entrancy check. Controller (UI) updates go back to main.
- **@synchronized guards:** the user dict, channel list, client list, `capabilitiesPending`, relations, and mode objects.
- **IRCUser removal timer:** fires on a global GCD queue and calls `client removeUser:` off-main (IRCUser.m:262-300).
- **Run-loop timers:** the `TLOTimer`s and `performSelectorInCommonModes` are main run-loop based. `onPongTimer` and `onRetryTimer` still wrap `disconnect` in a main-queue hop.

### Files Read

- `IRC/IRCClient.m`: 100-420, 571-640, 822-1010, 1150-1290, 2167-2656, 5569-6127, 6244-6600, 6733-6811, 7115-8128, 8128-8840, 8840-9030, 9065-9090, 9300-9415, 9531-9840, 9983-10002, 10291-10975, 11274-11790, 12872-13108
- `IRC/IRCWorld.m` (whole), `IRC/IRCTreeItem.m`, `IRC/IRCChannel.m` (whole), `IRC/IRCChannelMemberList.m` (whole), `IRC/IRCISupportInfo.m` (whole), `IRC/IRCChannelMode.m` (90-365), `IRC/IRCModeInfo.m` (119-128), `IRC/IRCConnection.m` (120-260, 395-543), `IRC/IRCMessage.m` (95-380), `IRC/IRCClientConfig.m` (grep of structure and keychain), `IRC/IRCServer.m` (grep), `IRC/IRCChannelConfig.m` (grep)
- `IRC/Users/IRCUser.m` (whole), `IRC/Users/IRCUserRelations.m` (whole), `IRC/Users/IRCChannelUser.m` (whole)
- Headers: `IRCClient.h`, `Private/IRCClientPrivate.h`, `IRCUser.h`, `Private/IRCUserPrivate.h`, `Internal/IRCUserInternal.h`, `IRCUserRelations.h`, `IRCChannelUser.h`, `IRCChannel.h`, `Private/IRCChannelPrivate.h`, `IRCChannelConfig.h`, `IRCISupportInfo.h`, `IRCWorld.h`, `Private/IRCWorldPrivate.h`, `IRCTreeItem.h`, `Private/IRCTreeItemPrivate.h`, `IRCConnection.h`, `IRCServer.h`, `IRCClientConfig.h`, `IRCPrefix.h`
- `Helpers/Cocoa (Objective-C)/NSStringHelper.m` (160-260)
- `Sources/Plugins/ZNC Additions/Classes/TPI_ZNCAdditions.m` (230-380)

### Boundaries

| Direction | What crosses |
|---|---|
| In: config | `TPCPreferences clientList` (array of dicts) → IRCClientConfig/IRCChannelConfig/IRCServer. Secrets go to the keychain under `textual.server.<serverId>`, `textual.nickserv.<clientId>`, `textual.proxy-server.<clientId>`, `textual.cjoinkey.<channelId>`. |
| Out: config | `IRCWorld save`/`savePeriodically`. Channel list changes rebuild `config.channelList` (`updateStoredChannelList`, C:612); utility windows are excluded, and queries are excluded unless `rememberServerListQueryStates`. |
| In: socket | IRCConnection XPC delegate protocol (IRCConnection.h:63-80). Data arrives as already-framed, decoded lines. |
| Out: socket | `sendLine` / `send:` (C:2659-2705) → `IRCConnection sendLine` → XPC `sendData` (with flood-control queue). |
| Out: UI | `mainWindow()` reload/updateTitle calls, `TVCLogController` printing (`print:by:inChannel:...`, C:5261), `postEventToViewController` (JS `Textual.handleEvent` tokens such as serverConnecting/Connected/Disconnected, channelJoined/Parted, channelMemberAdded/Removed, nicknameChanged), and member-list controller inserts/removes. |
| Out: notifications | `NSNotificationCenter`: IRCClient Will/DidConnect, WillSendQuit, Will/DidDisconnect, UserNicknameChanged, ChannelListWasModified; IRCWorld ClientListWasModified, WillDestroyClient/Channel. Also `notifyEvent` → TLONotificationController. |
| In/out: plugins | `THOPluginDispatcher interceptServerInput` (pre-dispatch rewrite), `postReceivedMessage` (filters that can suppress printing), `processBundlesServerMessage`. |
| Out: system | Reachability and sleep events enter through IRCWorld. |

### Non-Obvious Things

1. **No CASEMAPPING support at all.** Nicks and channels compare with Unicode case-insensitivity (C:984, C:2468, C:2383). On rfc1459 networks, `nick[]` and `nick{}` would be treated as different users. A Rust rebuild should parse CASEMAPPING and fold properly.
2. **CAP LS 302 multiline is broken.** For a continuation `CAP * LS * :caps`, `[m sequence:2]` includes the literal `*`, which is then rejected as unsupported. But `sendNextCapability` runs after **every** LS chunk (C:8615), so REQs start before the full list arrives. If the first chunk held no supported caps, `CAP END` goes out early. REQ also sends one cap per line rather than batching.
3. **SASL PLAIN uses `config.username` (ident) as both authzid and authcid**, not the nickname or a separate account field (C:8665-8668). The SASL password is `nicknamePassword` (the NickServ password). SASL failure does not abort; it continues with CAP END (C:10325-10345). Internal state such as `IsInSASLNegotiation`, `SASLPlainText` and `SASLExternal` lives inside the `capabilitiesPending` array alongside real pending caps, so `sendNextCapability` has to filter them (C:8390-8420).
4. **Reconnect has no backoff.** It is a fixed 20 s repeating timer, and it cycles servers round-robin on every connect (including Reconnect/Retry). `resetAllPropertyValues` clears `reconnectEnabled` right after the timer has been started (C:5662 vs C:5614). That works only because `onReconnectTimer` doesn't check the flag.
5. **IRCUser is a value object with shared identity.** Copies share `persistentStore` (relations and GC timer). Updating means copy, edit, `addUserAndReturn`, then relink all memberships to the new instance. Users are not deleted on QUIT/PART; a 5-minute GCD timer removes them once they have zero channel relations (IRCUser.m:46). Queries are not relations: `associateUser` ignores non-channels (IRCUserRelations.m:105).
6. **The member `modes` string must stay rank-sorted.** `highestRankedUserMode` is just `modes[0]` (IRCChannelUser.m:103). Sorting is maintained by `changeMember` via ISUPPORT rank, and by NAMES/WHO order (servers send highest first). Yet `isOp`/`isHalfOp` and `rankForModeSymbol` hardcode `qOao`/`h`/`v`/`yY` (IRCChannelUser.m:125-185) instead of using PREFIX.
7. **NAMES bug.** In RPL_NAMEREPLY, finding an existing member that isn't us does `return` (C:9779), which aborts the **rest of that 353 line**, not just that nick. NAMES is also ignored entirely once 366 has been seen, so a later `/names` doesn't refresh state. WHO looks up the member via the stale `user` instead of `userAdded` (C:9645). That is harmless only because relations are shared.
8. **Self JOIN on an already-active channel returns early** (C:7133-7137). Bouncer re-JOINs don't duplicate state, but also don't add self again. `IRCChannelStatusJoining` is set on JOIN send but triggers nothing; `resetStatus` ignores it (IRCChannel.m, `resetStatus`).
9. **Print-only (ZNC buffextras) messages.** These are synthetic JOIN/PART/QUIT/KICK/MODE/NICK with the channel as param 0 and `isPrintOnlyMessage=YES`. Handlers render them without mutating state. But `receiveNick` asserts `paramsCount == 1` (C:7558) while print-only NICK carries 2 params, so **buffextras nick changes are silently dropped**.
10. **server-time marks every tagged message `isHistoric`** (IRCMessage.m:333). Then `processIncomingMessageAttributes` (C:5950) advances `lastMessageServerTime` and, under the Playback cap, re-marks messages *newer* than the stored time as not historic. The persisted `lastMessageServerTime` is what drives `play * <ts>`.
11. **ZNC detection only works through CAP traffic** from `irc.znc.in` (C:2289). Without CAP from ZNC, `isConnectedToZNC` stays NO.
12. **Hardcoded network quirks:** Twitch (`*.twitch.tv`) disables WHO/ISON/default modes (C:952). freenode disables MONITOR (IRCISupportInfo.m:251). DALnet gets NickServ IDENTIFY to `NickServ@services.dal.net` (C:6764).
13. **NickServ identification is scraped from notice text** using token lists (C:6205-6213, C:6733-6809). `autojoinWaitsForNickServ` depends on it unless SASL succeeded (900).
14. **`replaceMember:withMember:resort:` ignores its `resort` argument** and always resorts (IRCChannelMemberList.m:432).
15. **`userIsIRCop` is set only by 381 RPL_YOUREOPER** (C:9983), never by our own UMODE `+o`. `userIsAway` comes from 305/306 (C:9065).
16. **The ERR range check `numeric > 400 && numeric < 597`** (C:8846) routes 401–596 to the error handler. SASL numerics (900–908) therefore stay in the main switch.

### Open Questions

- I did not read how the XPC helper (`RCMConnectionManager*`, the IRCConnectionHost target) frames lines, handles TLS validation, or runs the flood-control queue. I also didn't trace how the bad-certificate trust panel resumes a connection (IRCConnection.m:258-395 skimmed only).
- Not traced:
  - `updateConfig:updateSelection:` (C:415-570), which applies edited configs to a live client: server-list changes, channel add/remove.
  - The IRCClientConfig dictionary schema and migrations beyond the keychain service names.
- The rest of `receiveNumericReply` (WHOIS, LIST, ISON, MONITOR/WATCH 600-series, ban lists, 004 MYINFO) was only listed, not read line by line. 004 is printed only and not parsed.
- `IRCClientRequestedCommands` visible/hidden WHO bookkeeping, `IRCAddressBookUserTracking`, and `IRCMessageBatch` internals were not read in detail.
- I didn't verify whether the server sends `CAP NEW sasl` after registration, and whether re-authenticating mid-session works. `toggleCapability` with `isUpdateRequest` is effectively unused.
- I didn't confirm what the plugin `postReceivedMessage` return value does beyond suppressing printing.
