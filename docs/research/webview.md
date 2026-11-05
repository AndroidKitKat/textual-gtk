# Chat view: how Textual sets up its WKWebView, and how to reproduce it in WebKitGTK

Source tree: `~/workspace/Textual-4f09312` (commit 4f09312). The paths below are relative to `Sources/App/`.
`CV/` is short for `Classes/Views/Channel View/` and `JS/` for `Resources/Styling/JavaScript/API/`.
This report covers only the WKWebView (WebKit2) path. It skips the WebKit1 code inside `if (isUsingWebKit2 == NO)` branches.

Phase 1 needs live IRC lines in a WebKitGTK view, drawn by Textual's own style CSS/JS, templates and JS API.
The page-side half of this recipe was **run and verified** with `docs/research/webview-poc.py` (results in section 6).

## 1. Page construction

### 1.1 Where files live

- **App resources.** `TPCPathInfo applicationResources` is the `.app/Contents/Resources` folder. Xcode copies the folders `Styling/JavaScript`, `Styling/Style Default Templates` and `Styling/Bundled Styles` into it as folder references (`Textual App.xcodeproj/project.pbxproj:1180,1188,1227`). At runtime the paths are `<Resources>/JavaScript/API/...` and `<Resources>/Style Default Templates/Version 4/...`.
- **Template folder.** `<Resources>/Style Default Templates/Version <N>/`, where N is the style's `Template Engine Versions → default` (4 for every bundled style) (`Classes/Preferences/Themes/TPCTheme.m:867-883`).
- **Temporary copy of the style.** Textual copies the whole style folder to `<Caches>/Cached-Style-Resources/` (`TPCTheme.m:266-274`, `TPCThemeController.m:841-848`). It does this on every theme reload, together with a new random `cacheToken` (`TPCThemeController.m:786-791`). The page's base URL is this copy (`CV/TVCLogController.m:301-304`).
- **Varieties.** The style root is the "global variety". Each subfolder of `Varieties/` is one variety, which may contain `design.css`, `scripts.js`, `Templates/` (or the legacy `Data/Templates/`) and `settings.plist` (`TPCTheme.m:186-238, 1003-1048, 1094-1118`). The variety is chosen by its `Appearance` (`light`/`dark`) against the app's dark mode (`TPCTheme.m:755-817`).
- **File order** (`TPCTheme.m:597-702`):
  - CSS: global `design.css`, then the variety's `design.css`.
  - JS: global `scripts.js`, then the variety's `scripts.js`.
  - Templates: the variety's repository, then the global one, then the app's `Version 4` folder as fallback (`TPCTheme.m:911-972`).
  - All CSS and JS paths are remapped to the temporary copy (`temporaryCSSFilePaths` / `temporaryJSFilePaths`, `TPCTheme.m:842-850`).

### 1.2 The initial document

`-[TVCLogController initialDocument]` renders the Mustache template `baseLayout` (`CV/TVCLogController.m:1544-1603`). It is loaded by `setUp → loadInitialDocument → loadAlternateHTML:` (`:211-244`).

Data keys (`:1546-1600`, plus `generateOverrideStyle` `:1605-1645`):

| Key | Value on macOS | Used by baseLayout.mustache |
|---|---|---|
| `userConfiguredTextEncoding` | charset of the server encoding, e.g. `utf-8` | `<meta charset>` (line 3) |
| `activeStyleCSSFiles` | list of absolute paths (temporary copy) | `<link>` per item (5-7) |
| `cacheToken` | random integer string | `?{{cacheToken}}` query (6, 9, 19) |
| `applicationTemplatesPath` | `<Resources>/Style Default Templates/Version 4` | `baseLayout.css` link (9) |
| `applicationResourcePath` | `<Resources>` | core.js, corePrivate.js, `initializeCore` (11-15) |
| `activeStyleJSFiles` | list of absolute paths | `<script>` per item (18-20) |
| `userConfiguredFontName`, `userConfiguredFontSize` | font name; `pointSize * 72/96` | inline `<style>` (24-25) |
| `nicknameIndentationAvailable`, `predefinedTimestampWidth` | bool; px width of a sample timestamp plus `Indentation Offset` | hanging indent (28-37) |
| `userStyleSheetRules` | user CSS override (raw) | `{{{userStyleSheetRules}}}` (39) |
| `textDirectionToken` | `ltr`/`rtl` | `<body dir>` (45) |
| `appearanceToken` | `dark` if the style's `Underlying Window Color` is dark | `data-appearance` (49) |
| `usesCustomScrollers` | true only for WK2 with non-overlay scrollers and the pref on | `data-custom-scroller` (51-53) |
| `operatingSystemVersion` | e.g. `14.0` | `data-system-version` (56) |
| `isChannelView` / `isPrivateMessageView` / `isUtilityView`, `channelName` | flags and name | `data-view-name`, topic bar (58-78) |
| `viewTypeToken` | `server`, `channel`, `query`, ... | `data-view-type` (70) |
| `activeStyleAbsolutePath`, `configuredServerName`, `isReloadingStyle`, `appearanceDescription`, `sidebarInversionIsEnabled` | set, but the V4 baseLayout does not reference them | (for style overrides of baseLayout) |

The body skeleton has fixed IDs: `#loadingScreen`, `#topicBar` (channels only), and `#body > #messageBuffer` (`baseLayout.mustache:75-84`). The body has `onload="_Textual.viewBodyDidLoad()"` (`:47`).

### 1.3 How it is loaded

- `TVCLogView _loadHTMLString:baseURL:` writes the rendered HTML to `<temporaryURL>/<UUID>.html`. It then calls `-[WKWebView loadFileURL:allowingReadAccessToURL:themeController().temporaryURL]` (`CV/TVCLogView.m:235-268`). Before that, `recreateTemporaryCopyOfThemeIfNecessary` makes sure the copy exists (`:212-223`).
- The shared configuration (`CV/TVCLogViewInternalWK2.m:73-140`) sets:
  - `_allowUniversalAccessFromFileURLs = YES` (84)
  - `_allowFileAccessFromFileURLs = YES` (88)
  - `_developerExtrasEnabled = YES` (89)
  - one shared `WKUserContentController` with 40 script message handlers (95-134)
- The user agent is `Textual/1.0 (+https://help.codeux.com/textual/Inline-Media-Scanner-User-Agent.kb)` (`CV/TVCLogView.m:63`, `TVCLogViewInternalWK2.m:207`).
- Back/forward gestures and magnification are off (`:200-201`).
- **No `WKUserScript` exists anywhere.** All CSS and JS comes in through tags in the template.

### 1.4 Injection order (all through the template, parsed synchronously)

1. Style CSS `<link>`s: global `design.css`, then the variety's `design.css`. For Simplified that is only `Varieties/Light/design.css`, which runs `@import "inlineMedia.css"` (`Bundled Styles/Simplified/Varieties/Light/design.css:4`).
2. `<Resources>/Style Default Templates/Version 4/baseLayout.css`. It holds only the mIRC colour classes `.effect[data-foreground-color=N]` and the spoiler rule.
3. `<script>` `JavaScript/API/core.js`. It defines `var Textual = {}` and the no-op callbacks a style overrides.
4. `<script>` `JavaScript/API/corePrivate.js`. It defines `_Textual`, `Textual.initializeCore`, `includeScriptResourceFile` and `includeStyleResourceFile`.
5. Inline `Textual.initializeCore("<Resources>")`. While the document is still loading, this `document.write`s `<script>` tags (`JS/corePrivate.js:50-69, 88-103`), in this order:
   1. `private/core/clickMenuSelection.js`
   2. `private/core/documentBody.js`
   3. `private/core/events.js`
   4. `private/core/inlineMedia.js`
   5. `private/core/messageBuffer.js`
   6. `private/core/scrollTo.js`
   7. `private/scroller/state.js`
   8. `private/scroller/automatic.js`. It loads `automaticEmpty.js` instead only when `window.webkit` exists but `messageHandlers` is undefined (`:61-65`).
   9. `private/conversationTracking.js`
   10. `private/scriptSink.js`. It defines `app`, `appInternal` and `appPrivate`.
6. Style `<script>`s: global `scripts.js`, then the variety's `scripts.js`.
7. Inline `<style>` with the font, the indentation and `userStyleSheetRules`. Because it comes last, its `html, body { font-family }` rule overrides the style's.

## 2. Appending lines

### 2.1 Load handshake (before any line is accepted)

1. **Body `onload`.** `_Textual.viewBodyDidLoad()` (`JS/private/core/events.js:92-120`):
   - `_TextualScroller.bindToBestElement()` (`JS/private/scroller/state.js:361-387`) attaches the scroller to the first of `body`, `#body` or `#messageBuffer` whose `overflow-y` is not `hidden`. Simplified sets `body { overflow: hidden }` and `#body { overflow-y: auto }`, so it binds `#body`.
   - On WK2, a `requestAnimationFrame` follows, then `appPrivate.finishedLayingOutView()`, then **`Textual.viewBodyDidLoad()`**. Simplified implements that as `Textual.fadeOutLoadingScreen(1.00, 0.95)`. This is the only thing that turns `#body` from `opacity: 0` to visible.
2. **Native "finished loading".** It fires when KVO `loading == NO` and `didFinishNavigation` has happened, then waits a further 1.2 s (`CV/TVCLogViewInternalWK2.m:358-365, 540-579`). That calls `logViewWebViewFinishedLoading` (`CV/TVCLogController.m:1650-1700`). It sets `loaded = YES`, then evaluates:
   - `Textual.viewInitiated(viewType, clientUniqueId, channelUniqueId, channelName)` (`:1670-1675`)
   - `_Textual.viewFinishedLoading({selected, visible, reloadingTheme, textSizeMultiplier, scrollbackLimit})` (`:1681-1691`). This creates the `MutationObserver` that drives auto-scroll, sets `data-visible`/`data-selected` on `<body>`, calls `Textual.viewFinishedLoading()`, `changeTextSizeMultiplier`, and `_MessageBuffer.setBufferLimit` (`JS/private/core/events.js:122-161`).
   - `Textual.setTopicBarValue(topicText, topicHTML)` through `setInitialTopic` (`CV/TVCLogController.m:389-425`)
   - `reloadHistory` (`:549-631`). This ends with `_Textual.viewFinishedLoadingHistory()` (`:751-754`), plus `_Textual.documentBodyAppendHistoric(html, lineNumbers, isReload)` when there is scrollback (`:467-472`).
   - `printingQueue updateReadinessState:`. Queued print operations are not "ready" until `viewIsLoaded` (`CV/Extras/TVCLogControllerOperationQueue.m:262-316`).
3. Every native→JS call goes through `_evaluateFunction`, which **silently drops the call while `loaded == NO`** (`CV/TVCLogController.m:368-377`).

### 2.2 One new message

1. `-[TVCLogController print:completionBlock:]` (`CV/TVCLogController.m:1200-1314`) enqueues a block on the printing queue.
2. `renderLogLine:resultInfo:` (`:1316-1526`):
   - `TVCLogRenderer renderBody:` turns IRC formatting, links and nicks into HTML through the `formattedMessageFragment`, `renderedStandardAnchorLinkResource` and `renderedChannelNameLinkResource` templates (`CV/TVCLogRenderer.m:685-691, 879`).
   - It fills the line attributes (`:1372-1479`): `timestamp`, `formattedTimestamp`, `localizedTimestamp`, `isNicknameAvailable`, `nicknameColorStyle` (for example `hsl(h,s%,l%)`, `IRC/Users/IRCUserNicknameColorStyleGenerator.m:154`), `nicknameColorStyleOverride`, `nicknameColorHashingEnabled`, `formattedNickname`, `nickname`, `nicknameType` (`normal`/`myself`, `CV/TVCLogLine.m:285-292`), `lineType` (`privmsg`, `action`, `notice`, `join`, ...), `command`, `rawCommand`, `lineClassAttribute` (`text` for privmsg/action/notice, otherwise `event`), `highlightAttribute` (`"true"`/`"false"`), `message`, `formattedMessage` (raw HTML), `isHighlight`, `isRemoteMessage`, `isEncrypted`, `configuredServerName`, `inlineMediaEnabled`, `lineNumber`, `lineRenderTime`, `showDateIndicator`/`dateIndicatorMessage`, `showSessionIndicator`/`sessionIndicatorMessage`, and the path keys `activeStyleAbsolutePath` and `applicationResourcePath`.
   - The template is picked by `theme() templateWithLineType:` (`TPCTheme.m:885-904`). It looks for `Line Types/<lineType>` first, then the mapping in `Resources/Property Lists/TemplateLineTypes.plist`: `privmsg`, `action`, `notice` and `ctcp` use `newMessagePostedWithSender`, and every other type uses `newMessagePostedWithoutSender`.
   - `renderTemplate:` **removes all newlines** from the output (`CV/TVCLogRenderer.m:1059-1070`).
3. On the main queue it calls `appendToDocumentBody:withLineNumbers:@[lineNumber]` (`:1261`), which evaluates **`MessageBuffer.bufferElementAppend(html, [lineNumber])`** (`:379-384`).
   - `lineNumber` is `TVCLogLine.uniqueIdentifier`, the last 17 characters of a UUID, e.g. `9C60-0050E4C00067` (`CV/TVCLogLine.m:224-229`). The template turns it into `id="line-<lineNumber>"`.
4. The JS call is built as the source text `MessageBuffer.bufferElementAppend("<escaped>",["<id>"]);\n`. Strings are escaped only for `\`, `"`, `\r` and `\n` (`CV/TVCLogView.m:418-430, 613-683`). It runs with `evaluateJavaScript:completionHandler:` on the main queue (`:368-381`, `CV/TVCLogViewInternalWK2.m:407-434`).
5. In JS, `_MessageBuffer.bufferElementInsert("beforeend", ...)` (`JS/private/core/messageBuffer.js:129-159`) runs these steps:
   - It returns early if `_bufferBottomIsComplete === false`.
   - `buffer.prepareForMutation()` records whether the view is at the bottom (`JS/private/scroller/automatic.js:77-91, 157-180`).
   - `#messageBuffer.insertAdjacentHTML("beforeend", html)`.
   - It updates the buffer size and trims old lines (soft limit 200, hard limit 1000, `messageBuffer.js:56-66, 205-250`).
   - It calls `_Textual.messageAddedToView([lineNumber], false)`. That calls the style's `Textual.messageAddedToView(lineNumber, fromBuffer)` for each line, then `appPrivate.notifyLinesAddedToView(...)` → native (`events.js:170-182`).
   - The `MutationObserver` fires, then `TextualScroller.performScroll()`, which uses a `setTimeout(0)` and scrolls to the bottom if the view was at the bottom, auto-scroll is on and `document.hidden` is false (`automatic.js:104-144, 193-215`).

### 2.3 Other native→JS calls (all in `CV/TVCLogController.m`)

| Call | Line |
|---|---|
| `Textual.scrollToTopOfView(true)` | 432 |
| `Textual.scrollToBottomOfView(true)` | 437 |
| `_Textual.historyIndicatorAdd(html)` | 448 |
| `_Textual.historyIndicatorRemove()` | 456 |
| `Textual.scrollToHistoryIndicator()` | 461 |
| `Textual.jumpToLine(lineNumber)` | 721 |
| `_Textual.notifyDidBecomeVisible()` | 726 |
| `_Textual.notifySelectionChanged(bool)` | 739 |
| `_Textual.notifyDidBecomeHidden()` | 744 |
| `Textual.changeTextSizeMultiplier(x)` | 760 |
| `Textual.viewFontSizeChanged(bigger)` | 762 |
| `_MessageBuffer.setBufferLimit(n)` | 769 |
| `_InlineMediaLoader.processPayload(obj)` | 853 |

| Call | Where |
|---|---|
| `Textual.documentHTML()` | `CV/TVCLogView.m:116` |
| `Textual.clearSelection()` | `CV/TVCLogView.m:130` |
| `Textual.styleSettingDidChange(key)` | `TVCLogScriptEventSink.m:1592` |
| `Textual.appearanceDidChange(str)` | `IRC/IRCWorld.m:222` |

### 2.4 Callbacks a style's scripts.js may implement

All are defined as no-ops in `JS/core.js:67-115, 143, 292, 304`:

- `viewInitiated(viewType, clientHash, viewHash, viewName)`
- `messageAddedToView(lineNumber, fromBuffer)`, `messageRemovedFromView(lineNumber)`
- `historyIndicatorAddedToView()`, `historyIndicatorRemovedFromView()`
- `topicBarValueChanged(newTopic)`
- `viewFinishedLoading()`, `viewFinishedLoadingHistory()`, `viewFinishedReload()`
- `viewFontSizeChanged(bigger)`
- `viewPositionMovedToBottom/ToHistoryIndicator/ToLine/ToTop`
- `dateChanged(y, m, d)`
- **`viewBodyDidLoad()`**
- `appearanceDidChange(changedTo)`
- `handleEvent(token)`, sent only if `settings.plist` opts in
- `styleSettingDidChange(key)`, `preferencesDidChange()`
- `nicknameSingleClicked(e)` (`JS/private/core/clickMenuSelection.js:166`)

Callbacks the bundled styles actually set:

- **All seven styles** (`Bundled Styles/*/scripts.js`): `viewBodyDidLoad`, `messageAddedToView` and `nicknameSingleClicked`.
- **Equinox** also sets `viewInitiated`, `viewFinishedLoadingHistory` and `handleEvent`, and calls `app.channelIsJoined`, `app.localUserNickname` and `app.nicknameColorStyleHash`.

## 3. JS→native bridge

### 3.1 Registration and dispatch

- One shared `TVCLogScriptEventSink` is added as the script message handler for 40 names (`CV/TVCLogViewInternalWK2.m:95-134`).
- `userContentController:didReceiveScriptMessage:` maps a message's name to the selector `<name>:inWebView:` and invokes it with `message.body` and `message.webView` (`CV/TVCLogScriptEventSink.m:216-244`).
- `processInputData:...` (`:259-418`) parses the body:
  - **Dictionary.** It reads `promiseIndex` (number) and `values` (array). An argument check fails if `values` is missing when arguments are required.
  - **String or number.** It becomes `values = [x]`.
  - **Array.** It is used as `values`.
  - **null.** It becomes `values = [null]`.
  - On failure, `throwJavaScriptException` logs to the page console with `console.error` (`:441, 488`).

### 3.2 Async return values

- **JS side** (`JS/private/scriptSink.js:54-93`). `appInternal.makePromise(cb)` stores `cb` under an increasing integer, starting at 1. The call posts `{promiseIndex, values:[...]}` with `window.webkit.messageHandlers.<name>.postMessage(...)`.
- **Native side.** When `promiseIndex >= 0`, the handler's `completionBlock(value)` evaluates **`appInternal.promiseKept(promiseIndex, value)`**. A nil value becomes `null` (`CV/TVCLogScriptEventSink.m:393-404`). `promiseKept` calls the callback and deletes it (`scriptSink.js:57-68`).
- `app.isWebKit2()` is `window.webkit && typeof window.webkit.messageHandlers !== "undefined"` (`scriptSink.js:316-323`). When it is false, every call falls back to the WebKit1 global `TextualScriptSink`. That global does not exist in WebKitGTK, so each call throws `ReferenceError`.
- `appPrivate.setAutomaticScrollingEnabled` stays in JS on WK2 (`TextualScroller.setAutomaticScrollingEnabled`, `scriptSink.js:108-115`).
- `appPrivate.displayContextMenu` always posts to `messageHandlers` (`:193-196`).

### 3.3 Full list of handler names

Type means: N = notify (no reply), P = reply through promiseKept. The PoC column marks what a minimal "render messages" PoC needs. Handlers are in `TVCLogScriptEventSink.m:494-967` and `:978-1607`.

| Handler | JS caller (`scriptSink.js` line) | Body `values` | Type | Returns | PoC |
|---|---|---|---|---|---|
| `finishedLayingOutView` | appPrivate (99) | null | N | marks layout done; `TVCMainWindowChannelView` removes its overlay | **required** |
| `notifyLinesAddedToView` | appPrivate (274) | line id or array of ids | N | counts lines, plugin hook (`TVCLogController.m:803-820`) | **required** (no-op) |
| `notifyLinesRemovedFromView` | appPrivate (283) | ids | N | | **required** (no-op; fires once the buffer trims at 200 lines) |
| `logToConsole` | app (449) | message | N | system log | useful |
| `appearance` | app (504) | none | P | `"light"`/`"dark"` | phase 2 |
| `sidebarInversionIsEnabled` (deprecated) | app (489) | none | P | bool | phase 2 |
| `channelIsActive` | app (364); also `app.channelIsJoined` | none | P | bool | phase 2 (Equinox) |
| `channelMemberCount` | app (325) | none | P | int, throws if not a channel | phase 2 |
| `channelName` | app (384) | none | P | string or null | phase 2 |
| `serverChannelCount` | app (338) | none | P | int | phase 2 |
| `serverIsConnected` | app (351) | none | P | bool | phase 2 |
| `serverAddress` | app (397) | none | P | string | phase 2 |
| `networkName` | app (410) | none | P | string | phase 2 |
| `localUserNickname` | app (423) | none | P | string | phase 2 (Equinox) |
| `localUserHostmask` | app (436) | none | P | string or null | phase 2 |
| `inlineMediaEnabledForView` | app (476) | none | P | bool | phase 3 |
| `nicknameColorStyleHash` | app (517) | [nick, style] | P | colour or hash | phase 2 (Equinox) |
| `printDebugInformation`, `printDebugInformationToConsole` | app (467, 458) | message | N | prints a debug line | phase 2 |
| `styleSettingsRetrieveValue` | app (541) | [key] | P | value or null | phase 3 |
| `styleSettingsSetValue` | app (554) | [key, value] | P | bool; broadcasts `styleSettingDidChange` | phase 3 |
| `retrievePreferencesWithMethodName` | app (567) | [name] | P | value of a TPCPreferences getter | phase 3 |
| `sendPluginPayload` | app (530) | [label, content] | N | | phase 4 |
| `setURLAddress`, `setNickname`, `setChannelName`, `setSelection` | appPrivate (117-151) | value | N | context-menu target | phase 2 |
| `displayContextMenu` | appPrivate (193) | null | N | | phase 2 |
| `copySelectionWhenPermitted` | appPrivate (180) | none | P | bool | phase 2 |
| `channelNameDoubleClicked`, `nicknameDoubleClicked`, `topicBarDoubleClicked` | appPrivate (153-178) | null | N | join, query, edit topic | phase 2 |
| `renderMessagesBefore`, `renderMessagesAfter` | appPrivate (198, 211) | [lineNumber, max] | P | rendered lines for scrollback paging | phase 2 (history) |
| `renderMessagesInRange` | appPrivate (224) | [after, before, max] | P | | phase 2 |
| `renderMessageWithSiblings` | appPrivate (237) | [line, before, after] | P | | phase 2 (jump to line) |
| `notifyJumpToLineCallback` | appPrivate (263) | [line, ok, atBottom] | N | | phase 2 |
| `renderTemplate` | appPrivate (250) | [name, attrs] | P | HTML | phase 3 (inline media) |
| `loadInlineMedia` | appPrivate (292) | [addr, id, line, index] | N | | phase 3 |
| `encryptionAuthenticateUser` | appPrivate (303) | null | N | | OTR, open decision |

## 4. What a WebKitGTK port must replace

- **Message handlers.** Use `webkit_user_content_manager_register_script_message_handler(ucm, name, NULL)` for each of the 40 names, after connecting `script-message-received::<name>`. The page code expects `window.webkit.messageHandlers.<name>.postMessage`, and WebKitGTK provides exactly that, so `scriptSink.js` runs unchanged. Reply with `webkit_web_view_evaluate_javascript("appInternal.promiseKept(i, <json>)")`. Register every name even as a no-op: posting to a name that is not registered throws. `register_script_message_handler_with_reply` (2.40+) is not needed.
- **File access.** Set `allow-file-access-from-file-urls` and `allow-universal-access-from-file-urls` to TRUE, plus `enable-developer-extras`. The template links absolute paths outside the style folder (`<Resources>/JavaScript/...`). So load the page from a file: write `<cache>/Cached-Style-Resources/<UUID>.html` and call `load_uri("file://...")`. Do **not** use `load_html(html, base_uri)`: WebKitGTK terminates the web process when content references absolute local paths that are not children of `base_uri` (`WebKit-6.0.gir`, `load_html` doc).
- **Web process sandbox (blocker on koya).** WebKitGTK 6.0 has no `set_sandbox_enabled`, so the bubblewrap sandbox is always on. On koya (Ubuntu 24.04), `kernel.apparmor_restrict_unprivileged_userns = 1` and there is no AppArmor profile for bwrap. `bwrap --ro-bind / / true` fails with "setting up uid map: Permission denied", and a WebKit 6.0 view aborts with `Failed to fully launch dbus-proxy`. I measured this. The options are an AppArmor profile that grants `userns` to the app binary, a sysctl change, or `WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1` for development. This is Michael's call. Flatpak avoids the problem. Once the sandbox runs, the style copy and the app's resources may need `webkit_web_context_add_path_to_sandbox` (inferred, not tested).
- **Custom URL schemes.** The WK2 path registers none. `textual://` and `tview:` do not appear in any view code. Link clicks reach `decidePolicyForNavigationAction`. When `navigationType == LinkActivated` it cancels the navigation and opens the URL outside (`CV/TVCLogPolicy.m:300-313, 344-380`). `http`, `https` and `textual` open directly; other schemes ask for confirmation first. In GTK: handle the `decide-policy` signal for `WEBKIT_POLICY_DECISION_TYPE_NAVIGATION_ACTION` with `WEBKIT_NAVIGATION_TYPE_LINK_CLICKED`, then call `ignore` and `gtk_uri_launcher_launch`. Channel names are not links. They are `<span class="channel" ondblclick=...>`, which goes through the `setChannelName`/`channelNameDoubleClicked` handlers.
- **Dark mode.** The variety is picked from the app's appearance (`NSAppearanceNameDarkAqua`, `Controllers/TXAppearance.m:275-287`, `TPCTheme.m:755-817`). The `data-appearance` token comes from the style's `Underlying Window Color` (`TVCLogController.m:1596-1600`). Switching variety reloads the view, while `Textual.appearanceDidChange` is used only for styles that opt in. Replace the source with `AdwStyleManager:dark`.
- **Overlay until layout.** `TVCMainWindowChannelView` watches `layingOutView` and draws an overlay over the view until `finishedLayingOutView` arrives (`Views/Main Window/TVCMainWindowChannelView.m:369-445`). While the view is loading, the overlay is filled with the style's `Underlying Window Color` (`:585-589`). Otherwise it uses `Channel View Overlay Color`. This avoids a white flash on dark styles. GTK can show a `GtkOverlay` child, or set `webkit_web_view_set_background_color` to `Underlying Window Color`.
- **Fonts.** The bundled styles name macOS fonts ("Lucida Grande"). The template's inline style overrides `html, body` with the user font, so give it a Linux font. Keep the `pointSize * 72/96` conversion, or pass CSS pt directly.
- **Retina.** There is no code for it in the WK2 path. WebKitGTK follows the GTK scale factor.
- **Scrolling.** In WK2 every native scroller method is empty (`CV/TVCLogViewInternalWK2.m:439-482`). Scroll-to-bottom is done entirely in JS (the MutationObserver plus `TextualScroller`). It depends on `document.hidden` (`automatic.js:50-63, 127-144`). A mapped WebKitGTK view reported `hidden=false` in the PoC. Inferred, not tested: an unmapped view reports `hidden=true`, which matches how Textual uses visibility. Custom scrollbars apply only when overlay scrollbars are off, so use `usesCustomScrollers = false` on GTK.
- **Other native features.**
  - Context menu (`_webView:contextMenu:forElement:`) → the `context-menu` signal.
  - Key forwarding to the input field (`TVCLogView.m:140-158`) → `GtkEventControllerKey` in the capture phase.
  - File drag and drop (`TVCLogViewInternalWK2.m:593-616`) → `GtkDropTarget`.
  - Find (`_findString:`) → `WebKitFindController`.
  - Process crash handling → `web-process-terminated`.
  - User agent → `webkit_settings_set_user_agent`.
  - Process pool limit (`:142-176`) → not needed. All views share one `WebKitNetworkSession` and `WebKitWebContext`.
- **Mustache.** The templates use GRMustache's `isEmpty(...)` filter in sections (`{{^ isEmpty(localizedTimestamp) }}`). It appears in `newMessagePostedWithSender`, `newMessagePostedWithoutSender` and `formattedMessageFragment`, and in Sulaco's templates. They also use partials (`{{> dateIndicator}}`). The Rust engine must support both, or the templates need a preprocessing step. See `docs/research/templates.md`.

## 5. Minimal PoC recipe (Simplified, Light variety)

Why Simplified: its `scripts.js` uses only `fadeOutLoadingScreen` and `ConversationTracking`. It needs no `app.*` call that returns data. Its single design.css plus `inlineMedia.css` loaded without errors. Equinox would also need `channelIsJoined`, `localUserNickname` and `nicknameColorStyleHash`.

1. **Lay out the files once at startup.**
   - Copy `Styling/JavaScript` → `<res>/JavaScript`, and `Styling/Style Default Templates` → `<res>/Style Default Templates`.
   - Copy `Bundled Styles/Simplified` → `<cache>/Cached-Style-Resources/`.
2. **Resolve the variety.**
   - CSS: `[<cache>/.../Varieties/Light/design.css]`.
   - JS: `[<cache>/.../scripts.js]`.
   - Templates: `Varieties/Light/Templates`, then `<res>/Style Default Templates/Version 4`.
3. **Render `baseLayout.mustache`** with the keys in 1.2. Minimal values:
   - `userConfiguredTextEncoding=utf-8`
   - the two file lists, `cacheToken`, `applicationResourcePath=<res>`, `applicationTemplatesPath=<res>/Style Default Templates/Version 4`
   - `userConfiguredFontName`, `userConfiguredFontSize=9`, `nicknameIndentationAvailable=false`, `userStyleSheetRules=""`
   - `textDirectionToken=ltr`, `appearanceToken=light`, `usesCustomScrollers=false`
   - `isChannelView=true`, `channelName`, `viewTypeToken=channel`

   Write the result to `<cache>/Cached-Style-Resources/<UUID>.html`.
4. **Create the view.**
   - Settings: file access from file URLs, universal access, developer extras.
   - For each handler: connect `script-message-received::<name>`, then register. The minimal set is `finishedLayingOutView`, `notifyLinesAddedToView` and `notifyLinesRemovedFromView`, but registering all 40 costs nothing. Any message whose body has `promiseIndex` gets a reply with `appInternal.promiseKept(i, value)`.
   - Then `load_uri("file://<that html>")`.
5. **On `load-changed` FINISHED,** wait about 1.2 s, the way Textual does. Then evaluate, in order:
   1. `Textual.viewInitiated("channel","<client-id>","<channel-id>","#chan")`
   2. `_Textual.viewFinishedLoading({"selected":true,"visible":true,"reloadingTheme":false,"textSizeMultiplier":1.0,"scrollbackLimit":0})`
   3. `Textual.setTopicBarValue("<topic text>","<topic html>")`
   4. `_Textual.viewFinishedLoadingHistory()`

   Only after these, mark the view loaded and flush queued lines.
6. **For each IRC line:**
   - Render `newMessagePostedWithSender` (privmsg/action/notice) or `newMessagePostedWithoutSender` (anything else) with the attributes in 2.2.
   - Remove the newlines.
   - Evaluate `MessageBuffer.bufferElementAppend(<json html>, [<json lineNumber>])`. Build the arguments with a JSON serializer such as `serde_json`; it is stricter than Textual's escaping.
   - `formattedMessage` must already be HTML-escaped. Send it raw through the triple-brace slot.

## 6. Verification (measured)

`docs/research/webview-poc.py` runs this recipe with PyGObject. It uses a 60-line Mustache subset (sections, inverted sections, partials, `isEmpty()`) and appends 80 lines at 30 ms intervals. Screenshot: `docs/research/webview-poc.png`.

- **WebKitGTK 6.0 (2.52.6).** It aborted before loading. This is the bwrap/userns issue in section 4. Running with `WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1` was not tried; it needs Michael's sign-off.
- **WebKit2GTK 4.1, same 2.52.6 engine** (sandbox off by default; run with `POC_WEBKIT41=1`). All 40 handlers were registered. The probe after 80 appends returned:
  - `lines=80`, `#body opacity=1`, `#loadingScreen opacity=0`
  - `topic="Welcome to #test"`, `data-selected=true`
  - `isWebKit2=true`, `observer=true`, `isScrolledToBottom=true` (scrollTop 1070 of 1546)
  - handler traffic: `finishedLayingOutView` ×1, `notifyLinesAddedToView` ×80

  The screenshot shows Simplified's topic bar, coloured nicks and greyed join lines, scrolled to the newest line.
- **Only the three required handlers** (`POC_HANDLERS=...`): the same result.
- **No handlers at all:**
  - `app.isWebKit2()` is false.
  - `ReferenceError: TextualScriptSink` at `scriptSink.js:104` stops `viewBodyDidLoad` before the style's fade-in, so `#body` stays at `opacity: 0` and nothing is visible.
  - 80 more errors are caught inside `messageBuffer.js:156`.
- **One console error happens in every run:** `TypeError ... scrolledElement.scrollToBottom` at `state.js:324`. `automatic.js:227` runs the visibility callback at load, before `bindToBestElement`. It is the last statement of that file and does no harm. Inferred: macOS shows the same error.

The test does not yet cover: WebKitGTK 6.0 with a working sandbox, the Dark variety, real IRC formatting from `renderBody`, history paging (`renderMessagesBefore`), or `document.hidden` behaviour when switching views.
