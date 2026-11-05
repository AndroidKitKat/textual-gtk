# Phase 2 research: Textual's main window, selection, unread state and input

## Report: how Textual's main window is put together and wired to the IRC model (read-only, source at `~/workspace/Textual-4f09312`)

Paths below are relative to `~/workspace/Textual-4f09312/Sources/App/Classes/` unless they start with `~`. Mapping to the brief: there is no "TVCMainWindowTextViewMultiline" and no "TLONickCompletionStatus". The real classes are `TVCMainWindowTextView` and `TLONicknameCompletionStatus`. **IRCWorld has no selection API in this commit.** All selection state lives on `TVCMainWindow`.

### Components Found

- **TVCMainWindow** (`Views/Main Window/TVCMainWindow.m`). The NSWindow subclass, and the hub of the main window.
  - It holds outlets to the channel view, the input field, the split view, the member list and the server list (93-117).
  - It owns the selection state: `selectedItems` (the group of selected rows), `selectedItem` (the focused one) and the previous-selection ids (108-111).
  - It owns the input history manager, the nick completion object and the key handler (105-106, 114, created at 138-151).
  - It is also the server list's NSOutlineView data source and delegate (2153-2181, 2629-2817).
- **IRCWorld** (`IRC/IRCWorld.m`). Owns `clientList`. Factory and destructor for clients and channels. It creates one `TVCLogController` per client and per channel (634-647).
- **IRCTreeItem** (`Headers/IRCTreeItem.h:43-58`, `IRC/IRCTreeItem.m`). Base class of IRCClient and IRCChannel. It carries `viewController` (strong), the three counters `dockUnreadCount`, `nicknameHighlightCount` and `treeUnreadCount`, `isUnread` (true when `treeUnreadCount > 0`, 47-50) and `resetState` (sets all three to 0, 52-59).
- **IRCClient / IRCChannel** as tree items.
  - The client's children are its `channelList` (`IRCClient.m:1312-1320`); its label is `config.connectionName` (1322).
  - A channel's label is the custom `config.label`, or else its name (`IRCChannel.m:748-757`).
  - A client is active when `isLoggedIn` (`IRCClient.m:1297`); a channel is active when `status == Joined` (`IRCChannel.m:728`).
- **TVCServerList** (`Views/Server List/TVCServerList.m`). The NSOutlineView with insert/remove/move helpers (90-140), redraw helpers (145-260) and keyDown forwarding (381-404).
- **TVCServerListCell / RowCell** (`Views/Server List/TVCServerListCell.m`). Draws the label colour by state (213-337) and the unread badge (351-398).
- **TVCMemberList** (`Views/User List/TVCMemberList.m`). An NSTableView backed by an `IRCChannelMemberListController` (64, 114-147).
- **IRCChannelMemberListController** (`IRC/IRCChannelMemberListController.m`). An array controller. `assignToChannel:` makes the channel the table's dataSource and delegate, then pushes the member array into the controller (55-91).
- **IRCChannelMemberList** (`IRC/IRCChannelMemberList.m`). Per-channel sorted `memberContainer`, edited on a single serial queue (128-177) and kept sorted by binary-search insertion (182-206).
- **IRCChannelUser** (`IRC/Users/IRCChannelUser.m`). Holds the sort comparators (260-309) and the conversation "weights" used by nick completion (213-258).
- **TVCMainWindowChannelView** (`Views/Main Window/TVCMainWindowChannelView.m`). An NSSplitView that shows one subview per selected item (127-242). Clicking a dimmed pane changes the focused item (244-271, 499-517).
- **TVCMainWindowSplitView** (`Views/Main Window/TVCMainWindowSplitView.m`). The three-pane split: server list | content | member list. Has expand/collapse/toggle and saved positions (101-355).
- **TVCMainWindowTextView** (`Views/Main Window/TVCMainWindowTextView.m`), which subclasses **TVCTextViewWithIRCFormatter** (`Views/Input Text Field/TVCTextViewWithIRCFormatter.m`). A multi-line NSTextView that grows with its content (342-389). Return calls `textEntered` (254-263). It has its own key handler (96-128).
- **TLOInputHistory** (`Library/TLOInputHistory.m`). Input history, either per tree item or global; at most 100 entries (54).
- **TLONicknameCompletionStatus** (`Library/TLONicknameCompletionStatus.m`). The Tab completion state machine.
- **TLOKeyEventHandler** (`Library/TLOKeyEventHandler.m:150-190`). Maps (modifiers, keyCode or character) to a selector and invokes it on the target.
- **TXApplication** (`Controllers/TXApplication.m:72-117`). `sendEvent:` offers each key-down to the key window first, then to the first responder, before normal dispatch.
- **TXMenuController** (`Controllers/TXMenuController.m`). Builds the Cmd+1…0 channel menu (3043-3099) and routes Navigation menu tags to the main window's `select*` methods (3101-3191).
- **TXWindowController** (`Controllers/TXWindowController.m`). Only a registry of auxiliary windows and sheets, keyed by description string (94-211). Not relevant to the main window.
- **TXMasterController** (`Controllers/TXMasterController.m`). App lifecycle only. The main window calls its `applicationWakeStepOne` and `applicationWakeStepTwo` (`TVCMainWindow.m:189,205`).

### Flow

**1. Startup** (`TVCMainWindow _awakeFromNib`, 167-206)
- Calls `worldController() setupConfiguration`, which creates the clients.
- Then `setupTrees` (2153-2181):
  - sets itself as the server list's delegate and dataSource;
  - makes `menuController()` the member list's double-click target (2157-2158);
  - expands clients whose `config.sidebarItemExpanded` is set (2098-2105);
  - restores the selection saved under defaults key `"Window -> Main Window -> Server List Selection"`, a list of uniqueIdentifiers (2107-2126);
  - if nothing is saved, falls back to the first client that has autoConnect and is expanded, picking its first channel (2128-2151);
  - fakes `outlineViewSelectionDidChange:nil` (2177).

**2. Creating log controllers** (owned by each tree item)
- `IRCWorld createClientWithConfig:reload:` (530-570) does this:
  - `client.viewController = [self createViewControllerWithClient:client channel:nil]`;
  - builds the channels from the config, each with its own viewController (541-547);
  - adds the client to `self.clients`, then `serverList addItemToList:index inParent:nil`;
  - if this is the first client, `mainWindow select:client`;
  - repopulates the navigation menu.
- `createChannelWithConfig:...` (577-605) creates the channel's TVCLogController (586), then `client addChannel:`.
  - `addChannel` keeps channels above private messages: channels are inserted before the first non-channel (`IRCClient.m:1194-1225`).
  - Then `addItemToList:inParent:client` and `mainWindow adjustSelection`.
- Queries come from `createPrivateMessage:` (612-632), which activates the query if the client is logged in.
- The view controllers are never shared and never pooled. They live exactly as long as their tree item.

**3. Destroying**
- `destroyChannel:reload:part:` (714-751), in order:
  - posts `IRCWorldWillDestroyChannelNotification`;
  - parts the channel;
  - `selectOtherBeforeDestroy` (649-658), which calls `mainWindow deselect:`;
  - `channel prepareForPermanentDestruction` (`IRCChannel.m:474-499`): closes the log file, closes the channel's sheets, `inputHistoryManager destroy:self`, then `viewController prepareForPermanentDestruction`;
  - clears `client.lastSelectedChannel`;
  - removes the outline row, `client removeChannel`, then `adjustSelection`.
- `destroyClient:` (660-702) first quits if connected, and re-enters itself from `disconnectCallback`. Then `deselectGroup:`, which also deselects the children, and removes the row and the client.

**4. Selection changes**
- A user click goes to `outlineViewSelectionDidChange:` (2780-2817).
  - Two guard flags can make it return early (`ignoreNextOutlineViewSelectionChange`, `ignoreOutlineViewSelectionChanges`).
  - With Cmd held, the row under the mouse becomes the focused item.
  - Then `selectionDidChangeToRows:selectedItem:` (1579-1654):
    - `selectedItems = serverList.selectedObjects`;
    - if the group is unchanged, only refocuses (1588-1594);
    - otherwise `storePreviousSelection` (2301-2317);
    - `selectedItem` becomes the requested item if it is in the group, else the **last** selected row (1615-1619);
    - `updateChannelViewBoxContentViewSelection`, which is `channelView populateSubviews`;
    - `notifyDidBecomeHidden` for items that left the group, `notifyDidBecomeVisible` for items that joined (1632-1647);
    - then `selectionDidChangePostflight`.
- `selectionDidChangePostflight` (1656-1754) runs only if the focused item really changed. Steps:
  1. `resetState` on both the old and the new item. Leaving a channel also zeroes its counters.
  2. `notifySelectionChanged` on the old item's controller.
  3. If nothing is selected: `memberList assignToChannel:nil` and `updateTitle`, then stop.
  4. The server list's context menu follows the type: server, channel or query menu (1703-1709).
  5. `memberList assignToChannel:` with the channel, or nil for a server console or query (1712-1716).
  6. Focus the input field if the "focus on selection change" preference is on and VoiceOver is off (1719-1723).
  7. `inputTextField updateSegmentedController`.
  8. `inputHistoryManager moveFocusTo:item`, which swaps the draft text (see 6).
  9. `resetSpellingIgnores`, which drops nicks that completion had added to the spelling ignore list.
  10. The member-list split pane expands for channels (unless the user hid it) and collapses for consoles and queries (1735-1741).
  11. `notifySelectionChanged` on the new item.
  12. `storeLastSelectedChannel`, which sets `client.lastSelectedChannel`.
  13. Posts `TVCMainWindowSelectionChangedNotification`, updates the dock icon, `updateTitle`.
- Programmatic selection is `select:` (2397), `deselect:` (2405) and `deselectGroup:` (2414), all routed through `shiftSelection:toItem:options:` (2428-2573).
  - It expands the parent client.
  - If both old and new items are already in the group, it only refocuses (2466-2475).
  - Otherwise it builds a new row set. On deselect it picks the next row, or else the previous row, skipping a removed client's children (2517-2556).
  - Then `serverList selectRowIndexes:`, which fires the delegate path above.
- `adjustSelection` (2257-2299) re-applies `selectedItems` by row after inserts and removes.
- Clicking an already-selected row inside a multi-selection is caught in `selectionShouldChangeInOutlineView:` (2719-2769). It refocuses through `selectItemInSelectedItems:` and returns NO, so the group stays.
- At most 6 rows can be selected (2771-2778).

**5. Member list data**
- `assignToChannel:` goes to `IRCChannelMemberListController assignToChannel:` (55-84). It:
  - sets `tableView.dataSource` and `delegate` to the IRCChannel (the cell views come from `IRCChannel.m:701-715`);
  - detaches the old `IRCChannelMemberList`;
  - `newList assignController:self`, which on the main queue runs `replaceContents:self.memberList` (`IRCChannelMemberList.m:98-109`).
- After that, list changes are pushed in place:
  - adds: `addMember:` inserts into the container at the binary-search index on the serial queue, then `controller insertObject:atArrangedObjectIndex:` on main (260-307);
  - removes are the same pattern (320-347);
  - a mode change replaces the member with a re-sorted copy (451-544).
- The member list only exists after the channel is joined (`IRCChannel activate`, 418-436). If the channel is already selected at that point, activate assigns it to the member list itself (433-435).
- **Sort order** is `channelRankComparator`, which is `compareUsingRank:` (`IRCChannelUser.m:277-309`):
  1. If the `memberListSortFavorsServerStaff` preference is on, IRC operators come first.
  2. Then `channelRank`, highest first. The rank comes from the ISUPPORT PREFIX position: `IRCISupportInfoHighestUserPrefixRank - index`, or 0 if the mode is unknown (`IRCISupportInfo.m:516-529`). It is applied to `highestRankedUserMode`, which is simply `modes[0]` (110-119).
  3. Ties are broken by case-insensitive nick.
  - `modes[0]` works because `changeMember:mode:value:` inserts each new mode before the first lower-ranked one, so the string stays in rank order (492-521).

**6. Input history** (`TLOInputHistory.m`)
- A preference picks the scope: `inputHistoryIsChannelSpecific`. If set, there is one `TLOInputHistoryObject` per tree item, keyed by `uniqueIdentifier`. Otherwise one global object with key `"TLOInputHistoryDefaultObject"` (56, 205-230).
- `moveFocusTo:` (132-162) only applies when the history is per channel:
  - it saves the current field text into the old object's `lastHistoryItem`;
  - it loads the new item's saved draft, or clears the field.
  - So unsent drafts are per channel only in that mode.
- `add` (288-305) appends and sets `position = count`; the buffer is capped at 100 by dropping the oldest entry (307-316).
- `up:` (318-341) works like this:
  - If the field is not empty and differs from the entry at the current position, the field text is pushed into the buffer first. This keeps the draft.
  - Then `position -= 1`, clamped at 0, and the entry at that position is returned. At index 0 it returns nil, meaning no change.
- `down:` (343-372):
  - an empty field resets the position to the end and returns nil;
  - text that differs from the current entry is pushed and an empty string is returned;
  - otherwise it advances.
- `destroy:` drops the per-item history when an item is destroyed (107-130).
- Changing the scope preference either copies the global buffer to every item or wipes everything (164-203).
- Key bindings: Up and Down go through `moveInputHistory:checkScroller:YES` (`TVCMainWindow.m:1066-1105`). This only moves through history when the caret is on the first line (for Up) or the last line (for Down) of a multi-line input. Otherwise the arrow moves the caret. Ctrl+P and Ctrl+N move through history with no caret check (635-636).

**7. Tab completion** (`TLONicknameCompletionStatus.m`)
- Tab and Shift+Tab are registered at **window** level (`TVCMainWindow.m:622-623`). A `tabKeyAction` preference picks nick completion or "next unread channel" (1022-1042).
- `performCompletion:` (113-170) first focuses the field. It reuses the previous scan only if all of these hold:
  - the selection range equals the range left by the last completion;
  - the field text equals the text left by the last completion;
  - an index from the last completion exists.
  - Otherwise it runs `constructCache` (553-567).
- `constructCachedSearchPattern` (569-606) scans backwards from the caret to whitespace or a comma (`'\x02c'`). The search pattern is the text from there to the caret. `searchPatternIsAtStart` means the pattern starts at index 0.
- `constructCachedSearchPatternPrefixCharacter` (608-644) picks the mode:
  - a leading `/` **at start of line** completes a command and strips the `/`;
  - a leading `@` completes a nick and strips the `@`, which is put back later;
  - `#` completes a channel name, and the `#` stays in the pattern;
  - anything else completes a nick.
- `constructCachedCompletionSuffix` (646-735) works out which text after the caret gets replaced:
  - an active text selection is used as is;
  - for nicks, the user's `tabCompletionSuffix` (for example ": ") is searched forward, provided no whitespace lies between the caret and it;
  - otherwise, if `tabCompletionCutForwardToFirstWhitespace` is on, the text up to the next whitespace, `:` or `,` is cut.
- **Building the candidate list** (`performCompletion_step1`, 172-396):
  - An empty pattern is allowed only for nicks (177-179).
  - **Commands:** the lowercased local command index plus plugin and AppleScript commands, sorted alphabetically.
  - **Channels:** the current channel first, then the client's other channels in list order.
  - **Nicks:** only when a channel is selected; returns NO in a query or the server console (224-226).
    - With a non-empty pattern, members are sorted by `compareUsingWeights:`: total weight descending, ties by rank then nick (`IRCChannelUser.m:260-275`).
    - With an empty pattern, members are sorted alphabetically, and only the single highest-weight user is moved to the front, if any weight differs (234-258, 289-299).
    - Each nick is also added in a form trimmed of leading `^[]-_`{}\` characters, mapped back to the real nick (261-285). This is only done when the pattern is non-empty.
    - These names are appended last: NickServ, RootServ, OperServ, HostServ, ChanServ, MemoServ, the app name and the ISUPPORT network name (302-316).
  - Matching is a case-insensitive **prefix** match on the lowercase list (332-350).
  - Cycling: the first Tab picks index 0. Repeated Tab or Shift+Tab steps forward or backward with wrap-around (363-387).
  - The `@` prefix is put back on the result (389-391).
- **Weights** decay by half per minute (`decayConversation`, 241-258). When a member speaks, `IRCClient.m:6514-6521` adds:
  - +20 then +5 (`outgoingConversation`) if the line mentions your nick;
  - +4 then +1 (`conversation`) otherwise.
  - `incomingConversation` is never called.
- **Suffix** (step3, 408-470): only for a nick at the start of the line, the user's suffix is appended. If the suffix is empty, a space is appended unless `tabCompletionDoNotAppendWhitespace` is set. Whitespace already in place is not doubled. Other completions get one space.
- **Replacement** (step4, 472-523): `shouldChangeTextInRange` / `replaceCharactersInRange` / `didChangeText` over the pattern plus the old suffix, then the caret moves to the end, and the new ranges and text are cached for the next Tab.
- The completed word is added to the spelling ignore list (398-406).
- The state is cleared on send (`TVCMainWindow.m:1250`). Otherwise it is invalidated implicitly whenever the text or caret differs from what the last completion left.

**8. Typed line to IRC**
- Return in the text view: `doCommandBySelector:insertNewline:` calls `mainWindow textEntered` (`TVCMainWindowTextView.m:254-263`), which calls `inputTextAsCommand:Privmsg` (1243-1263).
- `inputTextAsCommand:` clears completion, takes the **attributed** string (so IRC formatting survives), empties the field, adds the text to history, then calls `inputText:asCommand:`.
- `inputText:asCommand:` (1265-1280) passes the text through `THOPluginDispatcher interceptUserInput:command:` (a plugin may rewrite it or swallow it with nil), then `selectedClient inputText:asCommand:`.
- Other send paths:
  - Cmd+Return / Cmd+Enter sends as an ACTION if `commandReturnSendsMessageAsAction` is set (1055-1064);
  - Ctrl+Enter sends if `controlEnterSendsMessage` is set, else it inserts a newline (1044-1053).
- `IRCClient inputText:asCommand:destination:` (`IRCClient.m:2721-2803`). The destination is `mainWindow().selectedItem` (2711):
  - Only PRIVMSG, ACTION and NOTICE are accepted as the command (2736-2741).
  - The text is split with `splitIntoLines`. More than 4 lines, or more than 4×510 characters, raises a suppressible "possible flood" alert (2759-2770). `TXMaximumIRCBodyLength = 510` (`Headers/IRC.h:39`).
  - On a **server console**: every line is a command; a leading `/` is stripped if present; then `sendCommand:` (2777-2785).
  - On a **channel or query**:
    - `/cmd` with length > 1, not starting with `//`, becomes `sendCommand:` without the slash;
    - `//text` sends "/text" as a message, since the first `/` is stripped (2796-2798);
    - a lone "/" is sent as text;
    - everything else goes to `sendText:asCommand:toChannel:`.
- `sendText:` (2810-2894):
  - refuses on utility windows;
  - per line, loops `stringFormattedForChannel:onClient:withLineType:`. This consumes the largest chunk that fits in 510 minus `:hostmask CMD #chan :` minus `\r\n`, using a 60-character hostmask guess if the real one is unknown, and prefers to break within 25 characters of the end (`IRCColorFormat.m:375-446, 619-631`);
  - encrypts if needed;
  - prints locally, unless echo-message is enabled and the text was not encrypted;
  - sends `PRIVMSG`/`NOTICE` with an ACTION wrapper when needed.
- `sendCommand:completeTarget:target:` (2989-4994):
  - strips `/`;
  - the first token is looked up in `IRCCommandIndex`, and a large switch handles local commands;
  - the target channel is the given name, or the selected channel if it belongs to this client (3027-3031);
  - unknown commands go to a plugin, then a script, then are sent raw as `UPPERCMD rest` (4944-4994).

**9. Unread and highlight**
- On incoming PRIVMSG, ACTION or NOTICE, the counters are bumped in the **print completion block**, after rendering, because the renderer decides `context.highlight` (`IRCClient.m:6445-6473`). They are bumped only if `notifyText` returns YES; when it returns NO (for example ignored users), nothing is counted.
- `setHighlightStateForChannel:` (2327-2340) and `setUnreadStateForChannel:isHighlight:` (2347-2369):
  - do nothing if the main window is key **and** the channel is the focused item. A channel that is visible but not focused in a split still counts.
  - `nicknameHighlightCount += 1`, then dock icon update and `reloadTreeItem`;
  - `dockUnreadCount += 1` only for non-channels (queries) unless the `displayPublicMessageCountOnDockBadge` preference is on;
  - `treeUnreadCount += 1`, and `refreshMessageCountForItem:` only if this is a highlight or the channel's `config.showTreeBadgeCount` is on.
- Queries do the same (6602, 6648-6651).
- The counters live on the IRCTreeItem. They are only ever bumped on channels and queries; clients are never counted.
- Reset happens on:
  - selection change (postflight above);
  - window becoming key, for the focused item (`resetSelectedItemState`, 403-416, called at 472);
  - mark-all-as-read (911-941), which also adds scrollback marks if `autoAddScrollbackMark` is on;
  - clear-contents (979-999).
- **Badge drawing** (`TVCServerListCell.m:351-398`):
  - the badge shows `treeUnreadCount`;
  - it is drawn when the row is not selected, or when it is selected but not focused in a multi-selection, or when the window is inactive;
  - it is suppressed by `showTreeBadgeCount == NO`;
  - highlight styling is used when `nicknameHighlightCount > 0` and the channel's `ignoreHighlights` is off.
- **Label colour** (213-337): selected, then active + highlight (a user custom colour if set), then active, then error on last join attempt, then disabled.

**10. Window title** (`updateTitle`, 1960-2061)
- Starts with the connection state prefix (disconnected, reconnecting, connecting, logged in or quitting), then nick, away marker and network name.
- For a server console it then adds the server address. For a channel: name, user count and mode string (password masked). For a query: nick plus `user.hostmaskFragment`. For a utility window: its name.
- **The topic is not in the title.** It goes to the WebView: `IRCChannel.m:306`, `viewController setTopic:`.
- `updateTitleFor:item` is a no-op unless that item is the focused one (1949-1958). It is called from many places in IRCClient (for example 7222, 7310, 7813).

### Keyboard shortcuts (menu items are in `Resources/User Interface/en.lproj/TXCMainMenu.xib:692-830`; the arrow keys are encoded as U+F700-F703)

| Shortcut | Action |
|---|---|
| Cmd+] / Cmd+[ | Next / previous channel (tags 7010000/1) |
| Cmd+Shift+] / [ | Next / previous active channel |
| Ctrl+Cmd+Tab / Ctrl+Shift+Tab | Next / previous unread channel |
| Cmd+Opt+Right / Left | Next / previous server |
| Ctrl+Opt+Right / Left | Next / previous active server |
| Ctrl+Tab | Move Forward (any row) |
| (none) | Move Backward |
| Opt+Tab | Previous Selection (also registered at window level, `TVCMainWindow.m:625`) |
| Cmd+Shift+Down / Up | Next / previous highlight |
| Ctrl+Cmd+B | Jump to Present |
| Cmd+1…9, 0 | First 10 channels **across all servers** in list order (`TXMenuController.m:3061-3076`) |

- Window-level bindings (`TVCMainWindow.m:615-651`):
  - Esc leaves fullscreen;
  - Cmd+B bold, Ctrl+Shift+U underline, Ctrl+Shift+I italic, Ctrl+Shift+C foreground colour, Ctrl+Shift+H background colour;
  - Cmd+. speech;
  - Ctrl+P / Ctrl+N history;
  - Opt+Cmd+L focuses the WebView.
- How navigation works (656-865):
  - it walks rows from the selected row with wrap-around, filtering by type (channel/query vs server) and by condition (all / `isActive` / `isUnread`);
  - if `channelNavigationIsServerSpecific` is on, the walk stays within the current client's children plus the client row (761-774).

### Files Read
- `IRC/IRCWorld.m`, `Headers/IRCWorld.h`, `Headers/Private/IRCWorldPrivate.h`
- `IRC/IRCTreeItem.m`, `Headers/IRCTreeItem.h`, `Headers/Private/IRCTreeItemPrivate.h`
- `Views/Main Window/TVCMainWindow.m` (most of it), `TVCMainWindowTextView.m`, `TVCMainWindowChannelView.m`, `TVCMainWindowSplitView.m` (method index only)
- `Views/Input Text Field/TVCTextViewWithIRCFormatter.m` (86-132)
- `Views/Server List/TVCServerList.m`, `TVCServerListCell.m`
- `Views/User List/TVCMemberList.m`
- `Views/Channel View/TVCLogController.m` (325-338, 570-590, 724-760)
- `Library/TLOInputHistory.m`, `Library/TLONicknameCompletionStatus.m`, `Library/TLOKeyEventHandler.m` (150-190)
- `IRC/IRCChannelMemberList.m`, `IRC/IRCChannelMemberListController.m`, `IRC/Users/IRCChannelUser.m` (105-340), `IRC/IRCISupportInfo.m` (516-529)
- `IRC/IRCClient.m` (1194-1340, 2290-2370, 2709-3120, 4940-5017, 6440-6521), `IRC/IRCChannel.m` (305-320, 415-499, 720-760)
- `Library/Color Formatting/IRCColorFormat.m` (375-470, 600-631), `Headers/IRC.h:39`, `Headers/TXSharedApplication.h:50`
- `Controllers/TXApplication.m`, `Controllers/TXMenuController.m` (3030-3200), `Controllers/TXMasterController.m` and `TXWindowController.m` (method index only)
- `Resources/User Interface/en.lproj/TXCMainMenu.xib` (528-830)

### Boundaries
- **Model to UI:** IRCClient calls straight into the main window through global accessors (`mainWindow()`, `mainWindowServerList()`, `mainWindowMemberList()`):
  - `reloadTreeItem:`, `refreshMessageCountForItem:`, `updateTitleFor:`, `isItemSelected:`, `selectedItem`, `select:`;
  - IRCChannelMemberList pushes rows into the controller;
  - IRCChannel `activate` assigns the member list;
  - IRCWorld adds and removes outline rows and calls `adjustSelection`.
  - There is no observer layer: the model and the UI are tightly coupled.
- **UI to model:** `TVCMainWindow inputText:`, then `IRCClient inputText:asCommand:destination:`, then `sendCommand:` / `sendText:`. Double-clicking the server list connects, quits, joins or parts depending on preferences (2578-2627).
- **UI to WebView:** `TVCLogController notifyDidBecomeVisible`, `notifyDidBecomeHidden` and `notifySelectionChanged`, which are JS calls `_Textual.*` (724-749). Lazy history loading depends on `visible` (578).
- **Plugins:** `THOPluginDispatcher interceptUserInput` before send; `sharedPluginManager findHandlerForOutgoingCommand` for unknown commands; plugin commands are offered in completion.
- **Notifications out:** `TVCMainWindowSelectionChangedNotification`, `IRCWorldWillDestroyClient/ChannelNotification`, `IRCWorldClientListWasModifiedNotification`.
- **Persistence:** the selection (uniqueIdentifiers in defaults), client expanded state (`config.sidebarItemExpanded`), split view visibility (1759-1787), and `clientList` / `channelList` order after drag-reorder (2901-2965).

### Non-Obvious Things
- **Selection has two layers.** `selectedItems` is the group: up to 6 rows, each shown as a pane in TVCMainWindowChannelView. `selectedItem` is the focused one that gets input, the member list and the title. Unread suppression, counter resets and `isItemSelected` all use the focused item only. A visible but unfocused pane keeps counting unread.
- **The default focus in a multi-selection is the *last* selected row** (1618), not the clicked row, unless Cmd is held or an already-selected row is clicked.
- **The input history dedup check is buggy** (`TLOInputHistory.m:299`): `if (lastEntry != nil || ![lastEntry.string isEqual...])` always adds, so consecutive duplicates are kept. Separately, `up:` and `down:` push the in-progress draft into the buffer, so drafts become history entries.
- **History navigation with arrows depends on the caret line** in the multi-line field (1066-1084). Ctrl+P and Ctrl+N skip that check.
- **Key dispatch order:** `TXApplication sendEvent` offers keys to the **key window's** handler before the first responder. So window-registered keys (Tab, Ctrl+P, Cmd+B…) win even while another control has focus. Typing in the server list or member list is redirected into the input field (592-613).
  - The server list swallows arrow and page keys without calling super (`TVCServerList.m:387-396`), so **arrow keys do not move the server-list selection**.
  - The member list does pass Up and Down through (`TVCMemberList.m:557-563`).
- **Nick completion only works in channels.** In queries and the server console it returns NO (224-226). Commands complete only at position 0. Matching is prefix-only. The trimmed-nick trick lets "foo" match "_foo". Weight ordering only applies when there is a pattern; an empty-pattern Tab is alphabetical, with only one "most talked to" user moved to the front.
- The weight method names are inverted: being *mentioned* calls `outgoingConversation` (`IRCClient.m:6517-6518`), and `incomingConversation` is never called.
- **Command-index completions go in lowercased**, so the original case is lost (192). The channel/command branch (321-325) rebuilds the lowercase list from the uppercase one in a non-obvious way.
- **Message splitting is byte-budget aware**, assuming a 60-character hostmask when the real one is unknown. The 4-line flood prompt fires on the raw input, before splitting.
- **On the server console every line is a raw command**, with or without a slash (2777-2785). In a channel, `//` escapes the slash.
- **Unread counters are bumped after the WebView render completes** (the print completion block), because the highlight decision comes from the renderer. Server consoles never get counts, so "unread" navigation never lands on a server row. A row's badge only refreshes when `showTreeBadgeCount` is on or for a highlight; otherwise the counter goes up silently.
- **Channels always sort above queries** within a client on insert (`IRCClient.m:1203-1221`). Drag-reorder enforces the same rule (2887-2895).
- **Cmd+1…0 counts channels across all servers in a flat list**, not per server.
- The member list is NSArrayController-backed, with the IRCChannel itself as table dataSource and delegate. `highestRankedUserMode` relies on the modes string being kept in rank order at insert time.
- `awakeFromNib` runs several times because of nib reloads, hence the static guard (157-164).

### Open Questions
- `updateTitleFor:client` only refreshes when the client row itself is focused. I did not confirm whether state changes reach the title while one of that client's channels is focused; `IRCClient.m:9072` calls `updateTitle` directly in at least one case.
- I did not trace the TVCMemberListCell drawing: mark, nick colouring, away dimming (`Views/User List/TVCMemberListCell.m`).
- I did not open `splitIntoLines` (`Frameworks/Cocoa Extensions/Classes/NSStringHelper.m`), so its exact handling of `\r\n` versus `\n` is unconfirmed.
- I did not read the rest of the `stringFormattedForChannel` algorithm (`IRCColorFormat.m:470-600`), nor how formatting codes are carried across chunks.
- I did not trace where `TVCServerList` gets `groupItems`, `itemsFromParentGroup` and `indexesOfItemsInGroup:` from; probably a TVCBasicTableView or outline category.
- I did not look into the segmented control in the input bar (`TVCMainWindowSegmentedControl.m`) or the loading screen.
- `TVCDockIcon.updateDockIcon` aggregation was seen only in grep (`Views/TVCDockIcon.m:66-69`).
