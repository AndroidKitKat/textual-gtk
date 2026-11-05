# Templates: what the Mustache files need, and how to render them in Rust

Research for phase 1 (render live IRC lines through Textual's original templates). Source tree: `~/workspace/Textual-4f09312` (all paths below are relative to it unless absolute). Written 2026-11-05.

Short version: the templates use spec Mustache plus exactly two GRMustache extensions: an "else" clause inside a section, and the `isEmpty(...)` filter. The else clause sits in `formattedMessageFragment`, which renders every fragment of every message body. Both `mustache` 0.9 and `ramhorns` 1.0.1 refuse to parse that file (tested, see section 5). Recommendation: a small custom renderer.

## 1. Constructs used

### Inventory

There are 22 `.mustache` files under `Sources/App/Resources/Styling`:

- 13 default templates in `Style Default Templates/Version 4/`.
- 2 in `Bundled Styles/Sulaco/Templates/` (`newMessagePostedWithSender`, `newMessagePostedWithoutSender`).
- 7 `encryptedMessageLock.mustache` overrides, one each in Astria, Equinox, Sapientia and Total Sublime, plus one per variety in `Simplified/Varieties/{Light,Dark}` and `Tomorrow/Varieties/{Light,Dark}`.

The Inline Content Loader adds 9 more, 3 in `XPC Services/Inline Content Loader/Resources/Modules/` and 6 in `Extensions/Core Media/Resources/Modules/*/`.

No bundled style has a `Line Types/` subdirectory.

All tags across all 31 files were counted with a scanner script (scratchpad `scan.py`; it regex-tokenizes `{{...}}` and classifies each tag):

| Construct | Count | Spec? |
|---|---|---|
| `{{var}}` (HTML-escaped) | 120 | spec |
| `{{{var}}}` | 9 | spec |
| `{{#section}}` | 54 | spec |
| `{{^inverted}}` | 5 | spec |
| `{{/close}}` | 58 | spec |
| `{{> partial}}` | 6 | spec |
| `{{! comment }}` | 7 | spec |
| `{{.}}` implicit iterator | 2 | spec |
| `{{^ isEmpty(x) }}` ... `{{/ isEmpty(x) }}` | 7 open + 7 close | **GRMustache filter** |
| `{{#x}}...{{^x}}...{{/x}}` else clause | 1 | **GRMustache extension** |
| `{{& x}}`, `{{=<% %>=}}`, `{{% pragma }}` | 0 | n/a |
| `{{< layout}}`, `{{$block}}` (inheritance) | 0 | n/a |
| dot paths `{{a.b}}` | 0 | n/a |
| `{{#each(x)}}`, `localize(...)`, other filters | 0 | n/a |

Tag delegates and custom rendering objects are not used either. The only GRMustache API calls are `templateRepositoryWithBaseURL:`, `templateNamed:error:`, `renderObject:error:` and `templateFromContentsOfURL:error:` (section 2). Nothing calls `extendBaseContext`, `GRMustacheFilter`, a tag delegate or `GRMustacheConfiguration`.

### The two GRMustache-only features

**1. Else clause inside a section.** This is the blocker. `Style Default Templates/Version 4/formattedMessageFragment.mustache:15-33`:

```
15  {{#fragmentTextColorUsesStyleTag}}
16      style=" ... color: {{fragmentForegroundColor}}; ... "
25  {{^fragmentTextColorUsesStyleTag}}
26      data-foreground-color="{{fragmentForegroundColor}}" ...
33  {{/fragmentTextColorUsesStyleTag}}
```

GRMustache 7 (the vendored binary is 7.3.2, `Frameworks/Static Libraries/Headers/libmustache/GRMustacheVersion.h`) reads `{{^x}}` inside an open `{{#x}}` as "else". A spec parser instead nests a new inverted section at line 25, and line 33 closes only that, so the outer section never closes. This file renders every message fragment, so without the else clause no message body renders at all.

Only the compiled GRMustache binary is vendored, not its source, so the else semantics come from GRMustache's documented behaviour. `GRMustacheTag.h:53` also mentions the related `{{^ name }}...{{/}}` empty-close form. The semantics are consistent with the template's intent: a hex colour goes in `style=`, an mIRC colour number goes in `data-*-color`, and `baseLayout.css` has 99 `data-foreground-color` selectors.

**2. The `isEmpty(...)` filter**, from the GRMustache standard library. The binary contains the `isEmpty`/`isBlank` strings. It is used only as `{{^ isEmpty(x) }}...{{/ isEmpty(x) }}`, meaning "x is a non-empty string", with spaces inside the tag:

- `Version 4/newMessagePostedWithSender.mustache:28,30` (`localizedTimestamp`) and `:49,51` (`nicknameColorStyle`).
- `Version 4/newMessagePostedWithoutSender.mustache:22,24` (`localizedTimestamp`).
- `Version 4/formattedMessageFragment.mustache:52,54` (`inlineNicknameColorStyle`).
- `Bundled Styles/Sulaco/Templates/newMessagePostedWithSender.mustache:36,38,50,52`.
- `Bundled Styles/Sulaco/Templates/newMessagePostedWithoutSender.mustache:20,22`.

A spec engine treats `isEmpty(localizedTimestamp)` as a missing key, so the inverted section always renders. That is wrong only when the value is empty: you get `title=""` or `style="color: ;"`. Rewriting it as `{{#x}}` works in engines where an empty string is falsy.

### Partials and name resolution

- `{{> dateIndicator}}` appears at `newMessagePostedWithSender:3` and `newMessagePostedWithoutSender:3`.
- `{{> sessionIndicator}}` appears at `newMessagePostedWithSender:67`, `newMessagePostedWithoutSender:33` and `messageBufferSessionIndicator:4`. The last file is a deprecated alias.
- `{{> encryptedMessageLock}}` appears at `newMessagePostedWithSender:33`.

GRMustache loads `{{>name}}` from `name.mustache` "located in the enclosing template's directory" in the same repository (`GRMustacheTemplateRepository.h:339-343`). So a partial resolves in the repository that supplied the parent template. It does not go through Textual's style-then-default fallback, which applies only to top-level `templateWithName:` calls (section 2).

Consequence: the 7 style `encryptedMessageLock.mustache` overrides are never rendered (inferred). The default `newMessagePostedWithSender` pulls in the default lock template, and the only style with its own `newMessagePostedWithSender` (Sulaco) uses `{{{encryptedMessageLockTemplate}}}` instead. No ObjC code sets that key (grep over `Sources/App/Classes` finds nothing), so it renders empty. This only matters for OTR/encrypted lines.

### Implicit iterator

`baseLayout.mustache:5-7` and `:18-20` loop over string arrays:

```
{{#activeStyleCSSFiles}}<link ... href="{{.}}?{{cacheToken}}">{{/activeStyleCSSFiles}}
```

`{{cacheToken}}` inside the loop resolves from the parent context, as in standard context-stack lookup.

### Inline Content Loader modules (lower priority)

All 9 files use spec Mustache only: variables, plain sections, and one `{{{unescapedHTML}}}` (`ICMInlineHTML.mustache:6`). They are loaded one file at a time with `templateFromContentsOfURL:`, with no repository and no partials (`XPC Services/Inline Content Loader/Classes/Service/ICLInlineContentModule.m:125`). They are rendered at `Modules/Root Classes/ICMInlineImage.m:141`, `ICMInlineVideo.m:154` and `ICMInlineHTML.m:59`.

## 2. Where ObjC renders them, and how lookup works

### Render entry points

| What | Where |
|---|---|
| One log line | `TVCLogController.m:1316` `renderLogLine:resultInfo:` builds `templateAttributes`. At `:1521` it calls `[theme() templateWithLineType:lineType]`, then `TVCLogRenderer renderTemplate:attributes:` at `:1523`. |
| Initial document | `TVCLogController.m:1546-1602` builds `templateTokens` (font and indent tokens come from `generateOverrideStyle`, `:1606`), then `renderTemplateNamed:@"baseLayout"`. It is loaded at `:234` with `loadAlternateHTML:`. |
| Body fragments and link resources | `TVCLogRenderer.m:653` `renderStringAsHTML:...` renders `renderedStandardAnchorLinkResource` (`:685`), `renderedChannelNameLinkResource` (`:691`) and `formattedMessageFragment` (`:879`). |
| All named renders | `TVCLogRenderer.m:1041` `renderTemplateNamed:attributes:` calls `[theme() templateWithName:]` (`:1045`), then `renderTemplate:` (`:1059`), which runs `[template renderObject:templateTokens error:NULL]` (`:1063`) and returns **`templateRender.removeAllNewlines`** (`:1069`). |
| Appending to the view | `TVCLogController.m:379` evaluates the JS function `MessageBuffer.bufferElementAppend(html, [lineNumbers])`. |

**Every render strips all newline characters from its output** (`NSStringHelper.m:447` in Cocoa Extensions replaces `newlineCharacterSet` with ""). That covers baseLayout, line templates and fragments. Spec rules for standalone-line whitespace therefore barely matter: only tabs and spaces survive. The Rust side must strip newlines the same way, or the DOM text will differ (for example, line breaks inside `<span class="message">`).

Templates render with GRMustache's default HTML content type. `{{x}}` escapes `& < > " '` (the binary contains `&amp; &lt; &gt; &quot; &apos;`), and `{{{x}}}` emits raw text. A missing key renders as "".

### Template lookup and fallback (`Preferences/Themes/TPCTheme.m`)

1. **`templateWithLineType:`** (`:885`) first tries `"Line Types/<lineTypeString>"` with errors silenced (`:889-891`). If that fails, it maps the line type through `Resources/Property Lists/TemplateLineTypes.plist` (`:894`):
   - `privmsg`, `action`, `notice` and `ctcp` map to `newMessagePostedWithSender`.
   - Every other line type (debug, join, part, quit, kick, kill, mode, nick, invite, topic, website, dcc-file-transfer, off-the-record-encryption-status) maps to `newMessagePostedWithoutSender`.
2. **`_templateWithName:`** (`:911`) checks an `NSCache` keyed by name (`:922`). On a miss it tries each repository in `templateRepositories` in order (`:953`), then the `defaultTemplateRepository` (`:962`). A not-found error (`GRMustacheErrorCodeTemplateNotFound` or Cocoa 260) means "try the next one" (`:936`).
3. **Repository order** (`:673-693`):
   - The selected variety's `Templates/`, if the selected variety is not the global variety.
   - Then the global (style root) variety's `Templates/`.
   - Then the default repository.
4. **Each variety's directory** comes from `_compatTemplatesAtURL:` (`:1107`). It uses legacy `Data/Templates/` if that exists, otherwise `Templates/`. Settings come from legacy `Data/Settings/styleSettings.plist` if it exists, otherwise `settings.plist` (`:1094`). Varieties are the subdirectories of `Varieties/` (`:234`, `:244`).
5. **The default repository** is `<app resources>/Style Default Templates/Version <templateEngineVersion>/` (`_applicationTemplateRepositoryURL`, `:867-876`). The same path, as `applicationTemplatesPath`, is where `baseLayout.css` is loaded from.

### Template-version logic

The settings key is `Template Engine Versions`, a dictionary (`TPCTheme.m:1224`):

1. Textual looks up the key for the app's short version string (`:1227-1235`).
2. If that is missing, it uses `default` (`:1238-1245`).
3. A value counts only if it lies in `[min, max]`, and both bounds are `TPCThemeSettingsNewestTemplateEngineVersion` = 4 (`TPCTheme.h:45`, `TPCTheme.m:49-50`).
4. If no valid value is found, it falls back to 4 and sets `usesIncompatibleTemplateEngineVersion` (`:1247-1253`).

All 7 bundled style roots declare `{default: 4}`. The Rust port can hard-code version 4 and keep the incompatibility flag.

Other `settings.plist` keys that feed rendering: `Appearance`, `Nickname Color Style` (`HSL-light`/`HSL-dark`, `:1201-1215`), `Nickname Format` (e.g. `%@%n:`), `Indentation Offset`, `Underlying Window Color`, `Channel View Overlay Color`, `Force Invert Sidebars`, `Post Textual.handleEvent() Notifications`. Varieties override them.

## 3. Data contract

Types: **bool** means GRMustache truthiness (NO/0 is falsy, and so are a missing key and an empty string). Strings are HTML-escaped by the template unless marked *raw*. "Set if" means the key is absent otherwise.

### newMessagePostedWithSender / newMessagePostedWithoutSender

Built in `TVCLogController.m:1316-1525`. All line references below are to that file unless noted.

| Key | Line | Meaning / format |
|---|---|---|
| `lineNumber` | 1463 | `logLine.uniqueIdentifier`, a string. A new one is the last 17 chars of a UUID, e.g. `9C60-0050E4C00067` (`TVCLogLine.m:224-229`). Used as `id="line-{{lineNumber}}"`. |
| `lineType` | 1410 | `TVCLogLine.stringForLineType` (`TVCLogLine.m:249-280`): `privmsg`, `action`, `notice`, `ctcp`, `join`, `part`, `quit`, `kick`, `kill`, `mode`, `nick`, `invite`, `topic`, `website`, `debug`, `dcc-file-transfer`, `off-the-record-encryption-status`. |
| `command` | 1412 | `logLine.command`. Defaults to `"-100"` (`TVCLogLine.m:60,148`), otherwise the IRC command or numeric. `rawCommand` (1413) is a legacy duplicate. |
| `lineClassAttribute` | 1417-1425 | `"text"` for privmsg/action/notice, else `"event"`. |
| `highlightAttribute` | 1429-1433 | The string `"true"` or `"false"`, taken from the renderer's keyword-match result. `isHighlight` (1441) is the same value as a bool and is not read by bundled templates. |
| `isRemoteMessage` | 1443 | bool, `memberType == Normal` (not the local user). |
| `nicknameType` | 1405 | `"myself"` for the local user, else `"normal"` (`TVCLogLine.m:285-292`). Set only when a nickname exists. |
| `timestamp` | 1382 | `receivedAt.timeIntervalSince1970`, an NSNumber (float seconds). |
| `formattedTimestamp` | 1384 | strftime of `receivedAt` (`TXGlobalModels.m:48`). The format is the style's, then the user's, then the default `[%H:%M:%S]` (`TVCLogLine.m:309-326`, `RegisteredUserDefaultsInContainer.plist:271-272`). |
| `localizedTimestamp` | 1386 | `TXFormatDateLongStyle(receivedAt, NO)`, a long-style localized date and time. Used for the `title=` tooltip through `isEmpty`. |
| `formattedMessage` | 1439 | *raw* HTML, the output of `TVCLogRenderer renderBody:` (section 4). |
| `message` | 1437 | Unrendered body. Not read by bundled templates. |
| `isNicknameAvailable` | 1393-1395 | bool. Not read by bundled templates. |
| `nickname` | 1404 | Raw nickname. Set if available. |
| `formattedNickname` | 1402 | `formattedNicknameInChannel:` trimmed. For action lines it is `"%@ "` (just the nick). For notice lines it is `-nick-` (`TVCLogLine.m:55-56,333-345`). Otherwise the style or user `Nickname Format` is expanded by `IRCClient.m:5142`: `%@` is the member's mode symbol (only in a channel), `%n` is the nick, `%%` is a literal `%`, and `%Nn` / `%-Nn` pad to width N. |
| `nicknameColorStyle` | 1397 | `logLine.nicknameColorStyle` (`TVCLogLine.m:387-402`). Computed only for privmsg and action lines, else nil. The value is `hsl(H,S%,L%)` from MD5 of `"a-" + lowercase(nick)` (`IRCUserNicknameColorStyleGenerator.m:57-181`: first 4 bytes as a native unsigned int; H = hash % 360; S and L from `>>1` and `>>2` in light/dark ranges, `:96-154`). A user override gives a hex colour instead. |
| `nicknameColorStyleOverride` | 1398 | bool, true when a user override supplied the colour. |
| `nicknameColorHashingEnabled` | 1400 | bool, the inverse of the `disableNicknameColorHashing` pref. |
| `isEncrypted` | 1447-1449 | Set to true only for encrypted (OTR or Blowfish) lines. |
| `inlineMediaEnabled` | 1362-1364, 1461 | bool, the view's inline-media pref and (privmsg or action). |
| `showDateIndicator`, `dateIndicatorMessage` | 1469-1473 | Set if `isFirstForDay && showDateChanges`. The message is `TXFormatDate(date, LongStyle, NoStyle)`, e.g. "November 5, 2026" (`:662-669`). |
| `showSessionIndicator`, `sessionIndicatorMessage` | 1475-1479 | Set on the newest line from the previous session. The message is the localized string `TVCMainWindow[4yo-mk]`. |
| `activeStyleAbsolutePath`, `applicationResourcePath` | 1374-1376 | File-system paths, used by `encryptedMessageLock` overrides. |
| `configuredServerName` | 1452-1457 | Network name. Not read by bundled templates. |
| `lineRenderTime` | 1465 | Render time as epoch seconds. (There is no `lineRenderType` key.) Not read. |

Sulaco also reads `encryptedMessageLockTemplate`, which is never set (section 1).

### dateIndicator, sessionIndicator

These are partials, so they share the line's context. They read `dateIndicatorMessage` and `sessionIndicatorMessage`, as above.

### baseLayout

Built in `TVCLogController.m:1546-1602` and `generateOverrideStyle` (`:1606-1641`):

| Key | Line | Meaning |
|---|---|---|
| `userConfiguredTextEncoding` | 1570 | IANA charset name of the server's primary encoding. |
| `activeStyleCSSFiles`, `activeStyleJSFiles` | 1554-1555 | Arrays of path strings to cached copies of `design.css` / `scripts.js`. Order is global variety, then selected variety (`TPCTheme.m:683-693`), copied under `~/Library/Caches/.../Cached-Style-Resources/`. |
| `cacheToken` | 1557 | Cache-busting query string from the theme controller. |
| `applicationTemplatesPath` | 1550 | Path to `Style Default Templates/Version 4`, for `baseLayout.css`. |
| `applicationResourcePath` | 1548 | App resources dir. `JavaScript/API/core.js` and `corePrivate.js` live under it, and it is passed to `Textual.initializeCore("...")`. |
| `userConfiguredFontName` | 1617 | Font family name. |
| `userConfiguredFontSize` | 1618 | `pointSize * 72/96`, a number emitted in `pt`. |
| `nicknameIndentationAvailable` | 1624-1627 | bool, false if `Indentation Offset` < 0 or RTL. |
| `predefinedTimestampWidth` | 1629-1639 | Pixel width of "now" formatted with the timestamp format in the channel font, plus `Indentation Offset`. |
| `userStyleSheetRules` | 1572 | *raw* user CSS from preferences. |
| `textDirectionToken` | 1590-1594 | `ltr` or `rtl`. |
| `appearanceToken` | 1596-1600 | `dark` or `light`, from `underlyingWindowColorIsDark` (brightness < 0.5, `TPCTheme.m:1260-1281`). |
| `usesCustomScrollers` | 1574 | bool (macOS scroller logic). |
| `operatingSystemVersion` | 1563 | OS version string (`data-system-version`). |
| `isChannelView`, `isPrivateMessageView`, `isUtilityView`, `channelName` | 1577-1583 | Set when the view has a channel. |
| `viewTypeToken` | 1585-1588 | `channel.channelTypeString`, or `"server"` for a server console. |
| (not read) | 1552-1568 | `activeStyleAbsolutePath`, `configuredServerName`, `isReloadingStyle`, `appearanceDescription`, `sidebarInversionIsEnabled`. |

### formattedMessageFragment

Rendered once per attribute run of the body (`TVCLogRenderer.m:653-880`, `renderFinalResultsAsHTML` `:884-911`). The open/close keys are stateful across fragments: the renderer remembers which effects are open in `_renderedBodyOpenAttributes`.

| Key | Line | Meaning |
|---|---|---|
| `messageFragment` | 877 | *raw* HTML for this run. One of:<br>- a rendered anchor resource or channel resource;<br>- an escaped fragment (`escapeString`);<br>- the raw fragment when the body is marked do-not-escape.<br>With escaping on, a leading or trailing space becomes `&nbsp;` (`:862-875`). |
| `fragmentIs{Bold,Italicized,Monospace,Struckthrough,Underlined}Opened` | 752-764 | Set on the first run where that effect is on. |
| `...ClosedAtEnd` | 765-769 | Set on the last run while the effect is still on. |
| `...ClosedAtStart` | 771-779 | Set on the first run after the effect turns off. |
| `fragmentTextColorOpened` | 828, 845 | Set when a new foreground or background colour starts. |
| `fragmentForegroundColor[IsSet]`, `fragmentBackgroundColor[IsSet]` | 829-847 | Colour string from `stringValueForColor:` (`:1104-1120`): an NSColor (hex colour code `\x04`) gives `#rrggbb` and `UsesStyleTag = YES`; an mIRC number (`\x03NN`) gives `"4"` and `UsesStyleTag = NO`, which baseLayout.css maps through `[data-foreground-color="4"]`. |
| `fragmentTextColorUsesStyleTag` | 831, 848 | bool, picks the branch of the else clause. |
| `fragmentTextColorClosedAtStart` / `ClosedAtEnd` | 806-810 | Set when the colour changes; ClosedAtStart is false on the first run. |
| `fragmentIsSpoiler` | 854-858 | Foreground == background colour string, privmsg or notice only. |
| `inlineNicknameMatchFound`, `inlineNicknameColorStyle`, `inlineNicknameUserModeSymbol` | 694-728 | For a run tagged as a channel member's nick ("conversation tracking"). Colour from `nicknameColorStyleForString:`; mode symbol only if the pref is on and the previous character is not already that symbol. |
| `nicknameColorStyleOverride` | (never set) | Read at template line 48 but never set in fragment tokens, so always falsy. |
| (not read) | 736, 755, 834-851 | `messageFragmentEscaped`, `fragmentIsBold`-style keys and `fragmentTextColor[IsSet]`, all set for backwards compatibility. |

### renderedStandardAnchorLinkResource

Built at `TVCLogRenderer.m:665-685`:

- `anchorLocation` is the scanner's URL (`link.stringValue`), HTML-escaped by `{{ }}`.
- `anchorTitle` is the fragment pre-escaped by `escapeString` and emitted raw.
- `anchorInlineMediaAvailable` and `anchorInlineMediaUniqueID` are set when inline media is on for the view. The ID is the link's `AHHyperlinkScannerResult.uniqueIdentifier`, de-duplicated per URL (`:322-325`).

### renderedChannelNameLinkResource

`channelName` is the fragment pre-escaped by `escapeString` (`:689`), then escaped again by `{{channelName}}`. That double escape is harmless in practice: the channel regex `#([a-zA-Z0-9\#\-]+)` (`:468`) cannot match characters that need escaping.

## 4. Escaping and body pre-rendering

The message body arrives at the template as **pre-rendered HTML** (`{{{formattedMessage}}}`). Mustache escapes only the metadata variables (nickname, timestamps, paths). `TVCLogRenderer renderBody:` (`TVCLogRenderer.m:941-997`) runs this pipeline:

1. **Plugin hook** `THOPluginDispatcher willRenderMessage:` may rewrite the body (`:959-963`).
2. **`stripDangerousUnicodeCharactersFromBody`** (`:282-304`) replaces runs of 3 or more combining diacritical marks with U+FFFD. It runs only if the pref is on, and only for privmsg, action, ctcp, dcc, notice and topic lines.
3. **`buildEffectsDictionary`** (`:113-265`) walks the control characters below 0x20, deletes them, and turns them into attributes on an `NSMutableAttributedString`:
   - Bold, italic (new and old codes), monospace, strikethrough and underline toggle on and off.
   - The mIRC colour code (`\x03fg[,bg]`) and the hex colour code are parsed by `colorComponentsOfCharacter:` (`Helpers/Cocoa (Objective-C)/NSStringHelper.m:404`).
   - Reset (`\x0F`) clears everything.
   - Background colour is kept on a bare colour code if a foreground is still set.

   The stripped text is saved as `OriginalBodyWithoutEffects`.
4. **`buildListOfLinksInBody`** (`:306-332`) runs `TLOLinkParser locateLinksInString:`, which wraps the Auto Hyperlinks framework's `AHHyperlinkScanner`, and adds a URL attribute per link. It is skipped for line types in `TLOLinkParser bannedLineTypes` (`TVCLogController.m:1326`).
5. **`matchKeywords`** (`:334-458`) handles highlight keywords, plain or regex, with excluded keywords. It sets `KeywordMatchFound`, which drives `highlightAttribute`.
6. **`findAllChannelNames`** (`:460-477`) tags `#name` matches as channel links, for privmsg, action and notice only. A match must be bounded by non-alphanumerics and must not overlap a URL.
7. **`scanBodyForChannelMembers`** (`:516-578`), for privmsg and action only, does a case-insensitive search for every channel member's nick, bounded by non-alphanumerics. It tags each match for conversation tracking. Highlight-spam detection (`:565-574`) cancels the highlight when nicks make up more than 75% of the body with more than 10 nicks, or more than 50% with more than 20.
8. **`renderFinalResultsAsHTML`** (`:884-911`) enumerates the attribute runs and renders each through `formattedMessageFragment`, as in section 3.

Escaping helpers:

- `escapeString` is `escapeHTML` (GTM `gtm_stringByEscapingForHTML`, `:1072-1083`) followed by `escapeStringSimple`, which turns a tab into 4 `&nbsp;` and two spaces into `&nbsp;&nbsp;` (`:1085-1102`).
- GTM escapes more than the five basic entities: it also turns non-ASCII characters into named entities. That is browser-equivalent, so the Rust port can escape just `& < > " '` and get identical rendering.
- A caller can turn body escaping off: `IRCClient.m:5350-5353` sets `DoNotEscapeBodyAttribute` when `print:...escapeMessage:NO`. Then the fragment is emitted raw. Which callers pass `escapeMessage:NO` was not traced (guess: app-generated HTML messages).

## 5. Engine recommendation

### Crates (checked 2026-11-05 with `cargo info` and the crates.io API)

| Crate | Version | Last release | License | Notes |
|---|---|---|---|---|
| `mustache` (rust-mustache) | 0.9.0 | 2018-02-12 | MIT/Apache-2.0 | Dynamic `Data` tree (`MapBuilder`). Unmaintained for 8 years. |
| `ramhorns` | 1.0.1 | 2024-10-16 | MPL-2.0 | Fast. Data goes through the `Content` trait (derive on structs). Partials only through `Ramhorns::from_folder`. |
| `handlebars` | 6.4.4 | 2026-08-12 | MIT | Handlebars, not Mustache. |

### Probe

The probe is a scratch crate (scratchpad `tplprobe/`) that parses the real Version 4 templates with each crate.

| Template | mustache 0.9 | ramhorns 1.0.1 | handlebars 6.4 |
|---|---|---|---|
| `formattedMessageFragment` | **`UnclosedSection("fragmentTextColorUsesStyleTag")`** | **`UnclosedSection(...)`** | parse error |
| `newMessagePostedWith[out]Sender` | renders, but `title=""` is emitted for an empty `localizedTimestamp` (isEmpty unsupported) | `PartialsDisabled` with `Template::new`; parses with `from_folder` | parse error (`isEmpty(...)`) |
| `baseLayout`, `dateIndicator`, link resources | OK | OK | `baseLayout` fails |

A load-time rewrite of the two extensions fixes parsing in both Mustache crates (verified: `formattedMessageFragment` renders `<span class="effect" style=" color: #ff0000; " > a &amp; b` with rust-mustache, and ramhorns parses it). The rewrite does two things:

- At line 25, `{{^x}}` becomes `{{/x}}{{^x}}`.
- `{{^ isEmpty(x) }}`/`{{/ isEmpty(x) }}` becomes `{{#x}}`/`{{/x}}`.

So the gap is exactly those two constructs and nothing else.

### Recommendation: a small custom renderer (about 300 lines plus tests)

The reasons below tie back to the features found:

1. **The required feature set is tiny and fully known.** It is: variables with GRMustache's escape set; triple; sections over bool, string and string-list values; inverted sections; the else clause; comments; `{{.}}`; partials; one filter (`isEmpty`) inside section tags with spaces; and the GRMustache truthiness rules (false/0, missing, empty string and empty list are falsy). No inheritance, delimiters, lambdas, dot paths or pragmas. A hand-written parser and renderer for this is small, and it can reject anything else with a clear error. Rejecting clearly matters in phase 4, when user styles arrive.
2. **Both Mustache crates fail on the template every message goes through.** Using either means keeping a source-rewriting pre-pass, which is a second, ad-hoc parser anyway. The rewrite is also lossy for user styles that might use `{{^}}`, `isBlank`, or the else form elsewhere.
3. **Lookup semantics are Textual's, not the crate's.** Top-level names fall back from variety, to style root, to `Version 4`. Partials resolve inside the parent's repository (section 1), and `Line Types/<type>` is tried before the plist map. Neither crate models multiple repositories. ramhorns ties partials to one folder; rust-mustache resolves partials against one search path.
4. **The data is a dynamic, mixed-type map per line** (strings, bools, numbers, string arrays, keys present or absent). That fits a `Value` enum and `HashMap<&'static str, Value>` directly. ramhorns wants derived `Content` structs, which is workable but awkward for "key absent" semantics across about 30 optional fields. rust-mustache's `Data` fits but is unmaintained.
5. **Output post-processing** (strip all newlines, `TVCLogRenderer.m:1069`) and per-name template caching (`TPCTheme.m:922`) are trivial to build in.
6. **Performance is a non-issue.** Each line renders 1 line template plus N fragment templates, a few hundred bytes each. Templates should be parsed once and cached.

Fallback if phase 1 needs pixels before the renderer exists: use `mustache` 0.9 with the two-rule rewrite above. It is proven on these templates, but replace it before user styles are supported. `handlebars` is ruled out: its syntax is incompatible with every `isEmpty` template and with `baseLayout`.

### Shape for the Rust side (for the architect step, not prescriptive)

- `enum Value { Bool(bool), Str(String), Num(f64), List(Vec<Value>), Map(...) }`. Truthiness follows GRMustache.
- `struct TemplateSet` holds the ordered repositories `[variety, style root, default V4]`. Each `Template` keeps its origin repository so partials resolve there. Each repository has a name-to-AST cache.
- `fn render(&Template, &Context) -> String` strips `\r` and `\n`.
- The body renderer (section 4) is a separate module producing `formattedMessage`. It is IRC-formatting parsing plus link, channel and nick tagging, and it calls the fragment template per run.
