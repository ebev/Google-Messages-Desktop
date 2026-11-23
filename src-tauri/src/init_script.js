// Injected into the main webview on load (including subframes) via
// `initialization_script_for_all_frames`; embedded into the binary with
// `include_str!` in lib.rs. Two placeholder tokens in the "State" section are
// replaced with the persisted settings before injection.
//
// What it does:
//  - Disables WebAuthn/passkeys so Google Sign-In skips broken embedded-webview
//    passkey prompts and goes straight to "Approve on another device" / password
//  - Observes the page title + conversation list for unread changes and calls
//    set_tray_badge
//  - Adds a gear button that opens the app settings window
//  - Routes target="_blank" links (in-window for Messages, OS browser otherwise)
//  - Wraps the Notification API (native bridge, "Show notifications" setting)
//  - Detects "open on another computer" and runs the background heartbeat
//
// Layout: every function is declared first (grouped by feature), then shared
// state, then the startup code that wires everything together.
(function () {
    // =====================================================================
    // Functions
    // =====================================================================

    // --- IPC & generic helpers ---
    function invoke(cmd, args) {
        if (window.__TAURI_INTERNALS__) {
            return window.__TAURI_INTERNALS__.invoke(cmd, args);
        }
        return Promise.reject(new Error('Tauri IPC unavailable'));
    }

    // Fire-and-forget invoke for commands whose failure is not actionable.
    function send(cmd, args) {
        invoke(cmd, args).catch(function () {});
    }

    function onMessagesHost() {
        return location.host === MESSAGES_HOST;
    }

    function isInAppHost(host) {
        return host === MESSAGES_HOST || host === 'accounts.google.com';
    }

    function onTrustedOrigin() {
        return isInAppHost(location.host);
    }

    function isElementVisible(el) {
        if (!el) return false;
        const r = el.getBoundingClientRect ? el.getBoundingClientRect() : null;
        return !!(r && r.width > 0 && r.height > 0);
    }

    // Returns the first element matched by any selector (tried in order) that
    // also passes the optional predicate. Only the first match of each
    // selector is considered.
    function queryFirst(selectors, predicate) {
        for (let i = 0; i < selectors.length; i++) {
            const el = document.querySelector(selectors[i]);
            if (el && (!predicate || predicate(el))) return el;
        }
        return null;
    }

    function childText(el, selector) {
        if (!el) return '';
        const child = el.querySelector(selector);
        return child && child.textContent ? child.textContent.trim() : '';
    }

    function styledElement(tag, css, text) {
        const el = document.createElement(tag);
        el.style.cssText = css;
        if (text) el.textContent = text;
        return el;
    }

    // --- Disable WebAuthn / Passkeys (runs in every frame) ---
    // Embedded webviews (WKWebView on macOS, WebKitGTK on Linux, WebView2 on
    // Windows) cannot satisfy third-party Relying Party / Associated Domain
    // attestation for google.com, so passkey challenges fail after the user
    // enters their email. By reporting that platform authenticators and
    // PublicKeyCredential are unavailable in every frame, accounts.google.com
    // skips the broken passkey step and goes directly to "Approve on another
    // device" (Google Prompt) or password sign-in.
    function resolveFalse() {
        return Promise.resolve(false);
    }

    function rejectPublicKeyRequests(method) {
        const orig = navigator.credentials[method];
        if (!orig) return;
        navigator.credentials[method] = function (options) {
            if (options && options.publicKey) {
                return Promise.reject(
                    new DOMException('Passkeys are not supported in this embedded webview.', 'NotSupportedError')
                );
            }
            return orig.apply(navigator.credentials, arguments);
        };
    }

    function disablePasskeys() {
        try {
            const pkc = window.PublicKeyCredential;
            if (pkc) {
                if (typeof pkc.isUserVerifyingPlatformAuthenticatorAvailable === 'function') {
                    pkc.isUserVerifyingPlatformAuthenticatorAvailable = resolveFalse;
                }
                if (typeof pkc.isConditionalMediationAvailable === 'function') {
                    pkc.isConditionalMediationAvailable = resolveFalse;
                }
                if (typeof pkc.getClientCapabilities === 'function') {
                    pkc.getClientCapabilities = function () {
                        return Promise.resolve({
                            conditionalCreate: false,
                            conditionalGet: false,
                            hybridTransport: false,
                            passkeyPlatformAuthenticator: false,
                            relatedOrigins: false,
                            signalAllAcceptedCredentials: false,
                            signalCurrentUserDetails: false,
                            signalUnknownCredential: false,
                            userVerifyingPlatformAuthenticator: false,
                        });
                    };
                }
            }
            try {
                delete window.PublicKeyCredential;
            } catch (_) {
                Object.defineProperty(window, 'PublicKeyCredential', {
                    value: undefined,
                    configurable: true,
                    writable: true,
                });
            }
            if (navigator.credentials) {
                rejectPublicKeyRequests('get');
                rejectPublicKeyRequests('create');
            }
        } catch (_) {}
    }

    // --- Unread badge & notification sync (title + DOM conversation list) ---
    function parseUnread(title) {
        const m = (title || '').match(/\((\d+)\)/);
        return m ? parseInt(m[1], 10) : 0;
    }

    function getConversationTitle(el) {
        return childText(el, '[data-e2e-conversation-name], .name, h2, h3');
    }

    function getConversationSnippet(el) {
        return childText(el, 'mws-conversation-snippet, [data-e2e-conversation-snippet], .snippet');
    }

    // Outgoing replies ("You: ...") mean the user already replied (e.g. from phone).
    function isOutgoingSnippet(snippet) {
        return /^You\s*:/i.test(snippet);
    }

    function isConversationRowSelected(el) {
        return !!(
            el.classList && el.classList.contains('selected') ||
            el.getAttribute('aria-selected') === 'true' ||
            el.querySelector('.selected, [aria-selected="true"]')
        );
    }

    function isConversationItemUnread(el) {
        if (!el) return false;
        if (isOutgoingSnippet(getConversationSnippet(el))) {
            return false;
        }
        const e2eAttr = el.getAttribute('data-e2e-is-unread');
        if (e2eAttr === 'true') return true;
        if (e2eAttr === 'false') return false;
        if (el.querySelector('[data-e2e-is-unread="true"]')) return true;
        if (el.querySelector('[data-e2e-is-unread="false"]')) return false;
        if (el.classList && el.classList.contains('unread')) return true;
        if (el.querySelector('.unread, .unread-badge, [data-e2e-unread-badge]')) return true;

        const ariaNodes = el.querySelectorAll('[aria-label]');
        let aria = el.getAttribute('aria-label') || '';
        for (let i = 0; i < ariaNodes.length; i++) {
            aria += ' ' + (ariaNodes[i].getAttribute('aria-label') || '');
        }
        if (/\bunread\b/i.test(aria)) return true;

        const textEl = el.querySelector('[data-e2e-conversation-snippet], mws-conversation-snippet, [data-e2e-conversation-name], .name');
        if (textEl && window.getComputedStyle) {
            const fw = parseInt(window.getComputedStyle(textEl).fontWeight, 10);
            if (fw >= 600) return true;
        }
        return false;
    }

    function inspectDomConversations() {
        const items = document.querySelectorAll('mws-conversation-list-item');
        const allTitles = new Set();
        const unreadTitles = new Set();
        let firstPreview = null;
        let topOrSelectedRepliedByYou = false;

        for (let i = 0; i < items.length; i++) {
            const el = items[i];
            const title = getConversationTitle(el);
            const snip = getConversationSnippet(el);
            if (title) allTitles.add(title);
            const isSelected = isConversationRowSelected(el);
            if ((i === 0 || isSelected) && isOutgoingSnippet(snip)) {
                topOrSelectedRepliedByYou = true;
            }
            if (isConversationItemUnread(el)) {
                if (title) unreadTitles.add(title);
                if (!firstPreview && (title || snip)) {
                    firstPreview = {
                        title: title || APP_TITLE,
                        body: snip || 'New unread message'
                    };
                }
            }
        }
        return {
            totalItems: items.length,
            unreadCount: unreadTitles.size,
            allTitles: allTitles,
            unreadTitles: unreadTitles,
            firstPreview: firstPreview,
            topOrSelectedRepliedByYou: topOrSelectedRepliedByYou
        };
    }

    function resetUnreadTracking() {
        lastTitleRawCount = 0;
        sawDomUnread = false;
        notifClosedByAppSinceIncrease = false;
    }

    function computeEffectiveUnread(titleCount, domInfo) {
        if (titleCount === 0) {
            resetUnreadTracking();
            return 0;
        }

        if (titleCount > lastTitleRawCount) {
            lastTitleRawCount = titleCount;
            notifClosedByAppSinceIncrease = false;
            if (domInfo.unreadCount > 0) {
                sawDomUnread = true;
            }
        }

        if (domInfo.totalItems > 0) {
            if (domInfo.unreadCount > 0) {
                sawDomUnread = true;
                return Math.min(titleCount, domInfo.unreadCount);
            }
            // All sidebar items are now read in the DOM, or the user replied on
            // their phone, or Google Messages closed its notification: clear the
            // stale unfocused (N) title count.
            if (sawDomUnread || notifClosedByAppSinceIncrease || domInfo.topOrSelectedRepliedByYou) {
                if (/^\(\d+\)\s*/.test(document.title)) {
                    document.title = document.title.replace(/^\(\d+\)\s*/, '');
                }
                resetUnreadTracking();
                return 0;
            }
        } else if (notifClosedByAppSinceIncrease) {
            return 0;
        }

        return titleCount;
    }

    function closeNotificationEntry(entry, notifyBackend) {
        if (!entry) return;
        activeNotifications.delete(entry.id);
        if (notifyBackend) {
            send('close_native_notification', { id: entry.id });
        }
    }

    // Closes every tracked message (non-takeover) notification that passes the
    // optional predicate.
    function closeMessageNotifications(predicate, notifyBackend) {
        const toClose = [];
        activeNotifications.forEach(function (entry) {
            if (entry.kind !== 'takeover' && (!predicate || predicate(entry))) {
                toClose.push(entry);
            }
        });
        for (let i = 0; i < toClose.length; i++) {
            closeNotificationEntry(toClose[i], notifyBackend);
        }
    }

    function hasMessageNotifications() {
        let found = false;
        activeNotifications.forEach(function (entry) {
            if (entry.kind !== 'takeover') found = true;
        });
        return found;
    }

    function clearAllMessageNotifications() {
        if (pendingFallbackTimer) {
            clearTimeout(pendingFallbackTimer);
            pendingFallbackTimer = null;
        }
        closeMessageNotifications(null, false);
        send('clear_native_notifications');
    }

    // Closes notifications for conversations the sidebar now shows as read.
    // Only acts on a positive signal: the notification's title must match a
    // sidebar conversation that is present and not unread. A title with no
    // matching row (group chats titled by sender, rows not rendered yet) is
    // left alone, and so is anything newer than the grace period, because the
    // notification can arrive before the sidebar row flips to unread.
    function pruneReadConversationNotifications(domInfo) {
        const now = Date.now();
        closeMessageNotifications(function (entry) {
            return now - entry.createdAt >= PRUNE_GRACE_MS &&
                domInfo.allTitles.has(entry.title) &&
                !domInfo.unreadTitles.has(entry.title);
        }, true);
    }

    // Safety-net notification for an unread increase that the web app did not
    // announce with its own Notification.
    function showUnreadFallbackNotification(count) {
        pendingFallbackTimer = null;
        if (Date.now() - lastWebNotifAt < 3000) return;
        if (lastUnreadCount === 0 || notifClosedByAppSinceIncrease) return;
        if (detectOtherSession()) return;
        const preview = inspectDomConversations().firstPreview;
        const title = preview ? preview.title : APP_TITLE;
        const body = preview
            ? preview.body
            : (count === 1 ? 'You have 1 unread message' : 'You have ' + count + ' unread messages');
        dispatchNotification(title, body, 'message', {
            tag: preview && preview.title ? ('gm-conv-' + preview.title) : 'gm-unread-fallback'
        });
    }

    function syncBadge() {
        syncRecentConversations();
        // The sidebar scan is the expensive part and is only needed while the
        // title reports unread messages, so skip it in the common idle case.
        const titleCount = parseUnread(document.title);
        const domInfo = titleCount > 0 ? inspectDomConversations() : null;
        const count = computeEffectiveUnread(titleCount, domInfo);

        const changed = count !== lastUnreadCount;

        if (count === 0) {
            // Clear once when the count drops to 0, and afterwards only if a
            // message notification slipped in without the count changing.
            // (A takeover notification alone must not trigger this.)
            if (changed || hasMessageNotifications()) {
                clearAllMessageNotifications();
            }
        } else if (domInfo.unreadCount > 0) {
            pruneReadConversationNotifications(domInfo);
        }

        if (!changed) return;
        const prev = lastUnreadCount;
        lastUnreadCount = count;
        send('set_tray_badge', { count });
        if (count === 0) return;

        // Only when the unread count increased while the window is
        // unfocused/hidden; the 1.5s delay gives the web app time to fire its
        // own Notification first.
        if (prev !== null && count > prev && window.__GM_SHOW_NOTIFICATIONS !== false && !document.hasFocus()) {
            if (pendingFallbackTimer) clearTimeout(pendingFallbackTimer);
            pendingFallbackTimer = setTimeout(function () {
                showUnreadFallbackNotification(count);
            }, 1500);
        }
    }

    function scheduleBadgeSync() {
        if (badgeSyncTimer) return;
        badgeSyncTimer = setTimeout(function () {
            badgeSyncTimer = null;
            syncBadge();
        }, 120);
    }

    function syncBadgeOnFocus() {
        syncBadge();
        setTimeout(syncBadge, 350);
        setTimeout(syncBadge, 1000);
    }

    function observeTitle() {
        const titleEl = document.querySelector('title');
        if (titleEl && titleEl !== observedTitleEl) {
            observedTitleEl = titleEl;
            titleObserver.observe(titleEl, { childList: true, characterData: true, subtree: true });
            syncBadge();
        } else if (!titleEl) {
            setTimeout(observeTitle, 500);
        }
        if (document.head) {
            titleObserver.observe(document.head, { childList: true });
        }
        if (document.body) {
            convObserver.observe(document.body, {
                childList: true,
                subtree: true,
                characterData: true,
                attributes: true,
                attributeFilter: ['data-e2e-is-unread', 'class', 'aria-label']
            });
        }
    }

    // --- Settings gear, docked left of the Google account avatar ---
    // The avatar is looked up by stable-ish attributes and re-checked as the
    // SPA re-renders; if it can't be found, fall back to a fixed position
    // near where it normally sits (top-right, left of the avatar).
    function openSettings() {
        invoke('open_settings').catch(function (err) {
            console.error('[gm] open_settings failed:', err);
        });
    }

    function setGearHover(btn, hovered) {
        btn.style.opacity = hovered ? '1' : '.82';
        btn.style.setProperty('background', hovered ? 'rgba(128,128,128,.32)' : 'rgba(128,128,128,.2)', 'important');
    }

    function makeGear() {
        const btn = document.createElement('button');
        btn.id = GEAR_ID;
        btn.type = 'button';
        btn.title = 'App settings';
        btn.setAttribute('aria-label', 'App settings');

        const svgNs = 'http://www.w3.org/2000/svg';
        const svg = document.createElementNS(svgNs, 'svg');
        svg.setAttribute('viewBox', '0 0 24 24');
        svg.setAttribute('width', '22');
        svg.setAttribute('height', '22');
        svg.setAttribute('fill', 'currentColor');
        svg.setAttribute('aria-hidden', 'true');
        svg.style.cssText =
            'width:22px !important;height:22px !important;min-width:22px !important;min-height:22px !important;' +
            'display:block !important;flex-shrink:0 !important;pointer-events:none !important;';

        const path = document.createElementNS(svgNs, 'path');
        path.setAttribute('d', GEAR_ICON_PATH);
        svg.appendChild(path);
        btn.appendChild(svg);

        btn.addEventListener('mouseenter', function () { setGearHover(btn, true); });
        btn.addEventListener('mouseleave', function () { setGearHover(btn, false); });
        btn.addEventListener('click', function (e) {
            e.preventDefault();
            e.stopPropagation();
            openSettings();
        });
        return btn;
    }

    function findAvatar() {
        const els = document.querySelectorAll(
            'a[aria-label*="Google Account"], a[href*="accounts.google.com"], a[href*="SignOutOptions"], ' +
            // Signed out, the same corner holds a "Sign in" button instead.
            'mw-ogbaccountswitcher button, button.sign-in');
        for (let i = 0; i < els.length; i++) {
            const r = els[i].getBoundingClientRect();
            if (r.width > 0 && r.top >= 0 && r.top < 150) return els[i];
        }
        return null;
    }

    // Cheap when already anchored (three property checks), so calling it on
    // every body mutation is fine — it keeps the gear alive across SPA
    // re-renders.
    function placeGear() {
        let btn = document.getElementById(GEAR_ID);
        if (btn && btn.isConnected && btn.dataset.anchored === '1') return;
        if (!btn) btn = makeGear();
        const avatar = findAvatar();
        if (avatar && avatar.parentElement) {
            btn.style.cssText = GEAR_BASE_CSS +
                'vertical-align:middle;align-self:center;margin:0 8px;';
            avatar.parentElement.insertBefore(btn, avatar);
            btn.dataset.anchored = '1';
        } else if (!btn.isConnected) {
            btn.style.cssText = GEAR_BASE_CSS +
                'position:fixed;top:14px;right:64px;z-index:2147483647;';
            btn.dataset.anchored = '0';
            document.body.appendChild(btn);
        }
    }

    // --- Desktop UI polish: suppress browser overscroll & ghost-dragging ---
    function injectDesktopStyles() {
        if (document.getElementById('gm-desktop-polish')) return;
        const style = document.createElement('style');
        style.id = 'gm-desktop-polish';
        style.textContent =
            'html, body { overscroll-behavior: none !important; }' +
            'img, button, [role="tab"], [role="menuitem"] {' +
            '  -webkit-user-drag: none; user-select: none;' +
            '}' +
            'a, [role="button"] {' +
            '  -webkit-user-drag: none;' +
            '}';
        (document.head || document.documentElement).appendChild(style);
    }

    function preventGhostDrag(e) {
        const t = e.target;
        if (t && (t.tagName === 'IMG' || t.tagName === 'A' || (t.closest && t.closest('a, button, [role="button"]')))) {
            e.preventDefault();
        }
    }

    // --- Route external & target="_blank" / window.open links ---
    // Handle link navigations so the main window stays on Google Messages:
    //   - messages.google.com (Messages / Calls / Voicemail) -> navigate in the
    //     same window, so they behave like tabs.
    //   - any other non-auth site (e.g. fi.google.com/account or links inside
    //     messages) -> open in the OS default browser via open_external, even if
    //     the anchor omits target="_blank" or sits inside a Shadow DOM.
    function parseUrl(url) {
        try {
            return new URL(url, location.href);
        } catch (e) {
            return null;
        }
    }

    function isHttpUrl(u) {
        return !!u && (u.protocol === 'https:' || u.protocol === 'http:');
    }

    function openExternal(href) {
        invoke('open_external', { url: href })
            .catch(function (e) { console.error('[gm] open_external failed:', e); });
    }

    // Returns true if the link was handled here.
    function route(u) {
        if (!isHttpUrl(u)) {
            return false;
        }
        if (u.host === MESSAGES_HOST) {
            window.location.assign(u.href);
        } else {
            openExternal(u.href);
        }
        return true;
    }

    function findAnchor(e) {
        if (e.target && e.target.closest) {
            const direct = e.target.closest('a[href]');
            if (direct) return direct;
        }
        const path = e.composedPath ? e.composedPath() : [];
        for (let i = 0; i < path.length; i++) {
            const el = path[i];
            if (el && el.tagName === 'A' && el.getAttribute && el.getAttribute('href')) {
                return el;
            }
        }
        return null;
    }

    function handleLinkClick(e) {
        if (!onTrustedOrigin()) return;
        if (e.type === 'auxclick' && e.button !== 1) return;
        const a = findAnchor(e);
        if (!a) return;
        const rawHref = a.getAttribute('href');
        if (!rawHref || rawHref.startsWith('#')) {
            if (e.type === 'auxclick') {
                e.preventDefault();
            }
            return;
        }
        const u = parseUrl(a.href);
        if (!isHttpUrl(u)) return;
        if (e.type === 'auxclick' && isInAppHost(u.host) && a.target !== '_blank') {
            e.preventDefault();
            return;
        }
        if (a.target === '_blank' || e.type === 'auxclick' || !isInAppHost(u.host)) {
            if (route(u)) {
                e.preventDefault();
                e.stopPropagation();
            }
        }
    }

    function routedWindowOpen(url) {
        if (onTrustedOrigin()) {
            const u = url ? parseUrl(url) : null;
            if (u && route(u)) {
                return null;
            }
        }
        return origOpen.apply(window, arguments);
    }

    // --- Cross-platform desktop keyboard shortcuts ---
    // Uses Cmd on macOS and Ctrl on Windows/Linux:
    //   Mod + ,        -> Open Settings window
    //   Mod + N        -> Start a new chat
    //   Mod + F / K    -> Focus conversation search
    //   Mod + 1..9     -> Switch to 1st..9th conversation in sidebar
    //   Mod + = / - / 0 -> Zoom in / out / reset
    //   Mod + R / F5   -> Reload
    function startNewChat() {
        const el = queryFirst(NEW_CHAT_SELECTORS);
        if (!el) return false;
        el.click();
        return true;
    }

    function focusAndSelect(input) {
        input.focus();
        if (input.select) input.select();
    }

    function focusSearch() {
        const visibleInput = queryFirst(SEARCH_INPUT_SELECTORS, function (inp) {
            return inp.offsetParent !== null;
        });
        if (visibleInput) {
            focusAndSelect(visibleInput);
            return true;
        }
        const btn = queryFirst(SEARCH_BUTTON_SELECTORS);
        if (!btn) return false;
        btn.click();
        setTimeout(function () {
            const inp = queryFirst(SEARCH_INPUT_SELECTORS);
            if (inp) focusAndSelect(inp);
        }, 50);
        return true;
    }

    function selectConversationByIndex(idx) {
        let items = document.querySelectorAll(
            'mws-conversation-list-item a, mws-conversation-list-item [role="listitem"], mws-conversation-list-item'
        );
        if (!items || items.length === 0) {
            items = document.querySelectorAll('a[href*="/web/conversations/"]:not([href$="/new"])');
        }
        if (items && items[idx]) {
            const el = items[idx];
            const target = el.querySelector ? (el.querySelector('a') || el) : el;
            target.click();
            return true;
        }
        return false;
    }

    function adjustZoom(action) {
        send('adjust_zoom', { action: action });
    }

    function handleKeydown(e) {
        if (e.key === 'F5' && !e.ctrlKey && !e.metaKey && !e.altKey) {
            e.preventDefault();
            window.location.reload();
            return;
        }
        // Suppress browser-only function keys on Windows/Linux (F3 find bar, F7 caret browsing)
        if (!isMac && (e.key === 'F3' || e.key === 'F7')) {
            e.preventDefault();
            return;
        }
        const mod = isMac ? (e.metaKey && !e.ctrlKey) : (e.ctrlKey && !e.metaKey);
        if (!mod || e.altKey) return;

        const key = e.key;
        if ((key === 'n' || key === 'N') && !e.shiftKey) {
            if (startNewChat()) {
                e.preventDefault();
            }
        } else if ((key === 'f' || key === 'F' || key === 'k' || key === 'K') && !e.shiftKey) {
            if (focusSearch()) {
                e.preventDefault();
            }
        } else if (!e.shiftKey && key >= '1' && key <= '9') {
            const idx = key.charCodeAt(0) - 49;
            if (selectConversationByIndex(idx)) {
                e.preventDefault();
            }
        } else if (!isMac) {
            // macOS handles Settings, Zoom, and Reload via the native app menu bar;
            // handling them here as well would fire them twice.
            if (key === ',' && !e.shiftKey) {
                e.preventDefault();
                send('open_settings');
            } else if (key === '=' || key === '+') {
                e.preventDefault();
                adjustZoom('in');
            } else if (key === '-' || key === '_') {
                e.preventDefault();
                adjustZoom('out');
            } else if (key === '0' && !e.shiftKey) {
                e.preventDefault();
                adjustZoom('reset');
            } else if ((key === 'r' || key === 'R') && !e.shiftKey) {
                e.preventDefault();
                window.location.reload();
            } else if (/^[pjuosgPJUOSG]$/.test(key)) {
                // Suppress Edge WebView2 print (Ctrl+P), downloads (Ctrl+J),
                // view-source (Ctrl+U), open/save file (Ctrl+O/S), find-next (Ctrl+G)
                e.preventDefault();
            }
        }
    }

    // --- Notifications ---
    function focusAppWindow() {
        send('focus_main_window');
    }

    // Minimal Notification-like object handed back to the web app so its
    // .onclick / addEventListener / .close() calls keep working.
    function createNotificationStub(onAppClose, tag, title, body) {
        const listeners = {};
        return {
            title: String(title || ''),
            body: String(body || ''),
            tag: String(tag || ''),
            onclick: null,
            onshow: null,
            onclose: null,
            onerror: null,
            close: function () {
                if (typeof onAppClose === 'function') {
                    try { onAppClose(); } catch (_) {}
                }
                if (typeof this.onclose === 'function') {
                    try { this.onclose(); } catch (_) {}
                }
                this.dispatchEvent({ type: 'close' });
            },
            addEventListener: function (type, fn) {
                if (!listeners[type]) listeners[type] = [];
                listeners[type].push(fn);
            },
            removeEventListener: function (type, fn) {
                if (!listeners[type]) return;
                listeners[type] = listeners[type].filter(function (f) { return f !== fn; });
            },
            dispatchEvent: function (evt) {
                const list = (evt && listeners[evt.type]) || [];
                for (let i = 0; i < list.length; i++) {
                    try { list[i](evt); } catch (_) {}
                }
                return true;
            }
        };
    }

    // Runs the web app's own click handlers for a notification (they open the
    // conversation), and stops tracking it since the OS dismisses it on click.
    function fireNotificationClick(entry, evt) {
        activeNotifications.delete(entry.id);
        const stub = entry.stub;
        if (typeof stub.onclick === 'function') {
            try { stub.onclick(evt); } catch (_) {}
        }
        stub.dispatchEvent(evt || { type: 'click' });
    }

    // Called from Rust (macOS / Linux) when a native notification is clicked.
    function nativeNotificationClicked(id) {
        const entry = activeNotifications.get(id);
        if (!entry) return;
        fireNotificationClick(entry, { type: 'click', target: entry.stub, preventDefault: function () {} });
    }

    function dispatchNotification(title, body, kind, options) {
        const opts = options || {};
        const normTitle = String(title || APP_TITLE).trim();
        const normBody = String(body || '');
        const effKind = kind || 'message';
        const tag = opts.tag
            ? String(opts.tag)
            : (effKind === 'takeover' ? 'gm-takeover' : ('gm-conv-' + normTitle));
        const id = tag;

        if (window.__GM_SHOW_NOTIFICATIONS === false) {
            return createNotificationStub(null, tag, normTitle, normBody);
        }
        if (effKind === 'message') {
            lastWebNotifAt = Date.now();
        }

        if (activeNotifications.has(id)) {
            closeNotificationEntry(activeNotifications.get(id), true);
        }

        const entry = {
            id: id,
            tag: tag,
            title: normTitle,
            kind: effKind,
            createdAt: Date.now(),
            stub: null
        };

        const stub = createNotificationStub(function () {
            // Invoked when Google Messages Web explicitly calls notification.close()
            // (e.g., when a message is read or replied to on the phone).
            closeNotificationEntry(entry, true);
            if (effKind === 'message') {
                notifClosedByAppSinceIncrease = true;
                scheduleBadgeSync();
                setTimeout(syncBadge, 400);
            }
        }, tag, normTitle, normBody);
        entry.stub = stub;

        activeNotifications.set(id, entry);
        send('send_native_notification', {
            id: id,
            title: normTitle,
            body: normBody,
            kind: effKind,
            // Reply / quick action / mark-as-read buttons are only offered when
            // the title identifies exactly one conversation in the sidebar.
            canReply: effKind === 'message' && findConversationRows(normTitle).length === 1
        });
        return stub;
    }

    // Shared by `new Notification(...)` and `registration.showNotification(...)`.
    function showMessageNotification(title, options) {
        const opts = options || {};
        return dispatchNotification(title, opts.body || '', 'message', opts);
    }

    // Replacement for window.Notification.
    function GMNotification(title, options) {
        return showMessageNotification(title, options);
    }

    function gmRequestPermission() {
        return Promise.resolve('granted');
    }

    function gmPermission() {
        return 'granted';
    }

    function swShowNotification(title, options) {
        showMessageNotification(title, options);
        return Promise.resolve();
    }

    function swGetNotifications(filter) {
        const tagFilter = filter && filter.tag ? String(filter.tag) : null;
        const list = [];
        activeNotifications.forEach(function (entry) {
            if (!tagFilter || entry.tag === tagFilter) {
                list.push(entry.stub);
            }
        });
        return Promise.resolve(list);
    }

    function installNotificationBridge() {
        GMNotification.requestPermission = gmRequestPermission;
        Object.defineProperty(GMNotification, 'permission', { get: gmPermission });
        window.Notification = GMNotification;

        if (typeof window.ServiceWorkerRegistration === 'function' && window.ServiceWorkerRegistration.prototype) {
            window.ServiceWorkerRegistration.prototype.showNotification = swShowNotification;
            window.ServiceWorkerRegistration.prototype.getNotifications = swGetNotifications;
        }
    }

    // --- Quick reply / mark as read from a notification ---
    // Google Messages has no API for this, so these drive the page: open the
    // conversation, then type and send. Nothing is typed until the open
    // conversation is confirmed to be the one the notification was for; on
    // any doubt the action is abandoned and the window is shown instead.
    function waitFor(check, timeoutMs) {
        return new Promise(function (resolve) {
            const deadline = Date.now() + timeoutMs;
            (function poll() {
                let result = null;
                try { result = check(); } catch (_) {}
                if (result) return resolve(result);
                if (Date.now() >= deadline) return resolve(null);
                setTimeout(poll, 100);
            })();
        });
    }

    function findConversationRows(title) {
        const rows = [];
        if (!title) return rows;
        const items = document.querySelectorAll('mws-conversation-list-item');
        for (let i = 0; i < items.length; i++) {
            if (getConversationTitle(items[i]) === title) rows.push(items[i]);
        }
        return rows;
    }

    function conversationRowLink(row) {
        return row.querySelector('a[href*="/web/conversations/"]');
    }

    // True once the page is showing this row's conversation: the URL matches
    // the row's link, or the row is marked selected.
    function isConversationOpen(row) {
        const link = conversationRowLink(row);
        if (link) {
            const u = parseUrl(link.getAttribute('href'));
            if (u && u.pathname === location.pathname) return true;
        }
        return isConversationRowSelected(row);
    }

    function findComposeBox() {
        return queryFirst(COMPOSE_BOX_SELECTORS, isElementVisible);
    }

    function composeText(box) {
        return (box.value !== undefined ? box.value : box.textContent) || '';
    }

    // Sets the text the way typing would, so the page's framework notices.
    function setComposeText(box, text) {
        box.focus();
        if (box.value !== undefined) {
            const proto = box.tagName === 'TEXTAREA' ? window.HTMLTextAreaElement.prototype : window.HTMLInputElement.prototype;
            const setter = Object.getOwnPropertyDescriptor(proto, 'value').set;
            setter.call(box, text);
        } else {
            box.textContent = text;
        }
        const input = typeof window.InputEvent === 'function'
            ? new window.InputEvent('input', { bubbles: true, inputType: 'insertText', data: text })
            : new Event('input', { bubbles: true });
        box.dispatchEvent(input);
        box.dispatchEvent(new Event('change', { bubbles: true }));
    }

    // Presses Enter in the message box, which is how the page sends by keyboard.
    function pressEnter(box) {
        box.focus();
        ['keydown', 'keypress', 'keyup'].forEach(function (type) {
            box.dispatchEvent(new KeyboardEvent(type, {
                key: 'Enter', code: 'Enter', keyCode: 13, which: 13, bubbles: true, cancelable: true
            }));
        });
    }

    // Resolves to true once the page has emptied the box, which it does when
    // it has taken the message.
    function waitForSent(box, timeoutMs) {
        return waitFor(function () { return !composeText(box).trim(); }, timeoutMs);
    }

    // Asks the app for a real Return key press in this window, which the page
    // treats exactly like the user's own. Resolves to false where the app
    // cannot do that.
    function pressEnterNatively(box) {
        box.focus();
        return invoke('press_enter_key').then(
            function (pressed) { return pressed === true; },
            function () { return false; }
        );
    }

    // Sends what is in the message box, trying one way after another until the
    // page empties the box: a real Return key press, the Send button, a
    // simulated Return, and last a real Return with the window showing. Each
    // attempt runs only while the box still holds the text, so the message
    // cannot go out twice.
    function sendComposedMessage(box) {
        const attempts = [
            function () { return pressEnterNatively(box); },
            function () {
                return waitFor(function () { return findSendButton(box); }, SEND_STEP_TIMEOUT_MS).then(function (btn) {
                    if (btn) btn.click();
                    return !!btn;
                });
            },
            function () { pressEnter(box); return true; },
            function () {
                focusAppWindow();
                return waitFor(function () { return document.hasFocus(); }, SEND_STEP_TIMEOUT_MS).then(function () {
                    return pressEnterNatively(box);
                });
            }
        ];
        function attempt(i) {
            if (!composeText(box).trim()) return Promise.resolve(true);
            if (i >= attempts.length) return Promise.resolve(false);
            return Promise.resolve(attempts[i]()).then(function (tried) {
                if (!tried) return attempt(i + 1);
                return waitForSent(box, SEND_STEP_TIMEOUT_MS).then(function (sent) {
                    return sent ? true : attempt(i + 1);
                });
            });
        }
        return attempt(0);
    }

    // A short description of the message box and the buttons around it, for
    // the app's debug log. It carries no message text.
    function describeComposeArea(box) {
        const scope = (box.closest && box.closest('mws-message-compose, mw-message-compose')) || document;
        const buttons = [];
        const found = scope.querySelectorAll('button');
        for (let i = 0; i < found.length && i < 12; i++) {
            const b = found[i];
            buttons.push('<button aria-label="' + (b.getAttribute('aria-label') || '') + '" class="' + (b.className || '') + '"' +
                (b.disabled || b.getAttribute('aria-disabled') === 'true' ? ' disabled' : '') + '>');
        }
        return ('send failed: box=' + box.tagName + ' focused=' + (document.activeElement === box) +
            ' pageFocus=' + document.hasFocus() + ' visibility=' + document.visibilityState +
            ' scope=' + (scope === document ? 'document' : scope.tagName) + ' buttons=' + buttons.join(' ')).slice(0, 1500);
    }

    function findSendButton(box) {
        const scope = (box.closest && box.closest('mws-message-compose, mw-message-compose')) || document;
        for (let i = 0; i < SEND_BUTTON_SELECTORS.length; i++) {
            const btn = scope.querySelector(SEND_BUTTON_SELECTORS[i]);
            if (btn && !btn.disabled && btn.getAttribute('aria-disabled') !== 'true') return btn;
        }
        return null;
    }

    // Opens the one conversation with this title and resolves to its sidebar
    // row once the page is showing it, or to null if that cannot be confirmed
    // (no row, several rows with the same name, or the page never switched).
    function openConversationByTitle(title) {
        const rows = findConversationRows(title);
        if (rows.length !== 1) return Promise.resolve(null);
        const row = rows[0];
        if (!isConversationOpen(row)) {
            (conversationRowLink(row) || row).click();
        }
        return waitFor(function () {
            // Re-check the title too: the list can re-render while we wait.
            return isConversationOpen(row) && getConversationTitle(row) === title ? row : null;
        }, ACTION_TIMEOUT_MS);
    }

    // Tells the user an action from a notification did not go through: shows
    // the window and posts a notification carrying the unsent text.
    function reportActionFailure(title, text, reason) {
        console.warn('[gm] notification action failed:', reason);
        focusAppWindow();
        if (text) {
            dispatchNotification('Reply not sent', 'To ' + title + ': ' + text, 'message', { tag: 'gm-reply-failed' });
        }
        return false;
    }

    // Resolves to true if the message was sent.
    function replyToConversation(title, text) {
        const message = String(text || '').trim();
        if (!message) return Promise.resolve(false);
        return openConversationByTitle(title).then(function (row) {
            if (!row) return reportActionFailure(title, message, 'conversation not found or not confirmed');
            return waitFor(findComposeBox, ACTION_TIMEOUT_MS).then(function (box) {
                if (!box) return reportActionFailure(title, message, 'no message box');
                // Never overwrite or send a draft the user already started.
                if (composeText(box).trim()) return reportActionFailure(title, message, 'message box already has a draft');
                setComposeText(box, message);
                return sendComposedMessage(box).then(function (sent) {
                    if (sent) return true;
                    // The text stays in the box as a draft; the window is
                    // already showing from the last attempt.
                    console.warn('[gm] notification action failed: message box did not clear after send');
                    send('log_page_diagnostic', { message: describeComposeArea(box) });
                    return false;
                });
            });
        });
    }

    // Opening the conversation is what marks it read. Resolves to true if it opened.
    function markConversationRead(title) {
        return openConversationByTitle(title).then(function (row) {
            if (!row) return reportActionFailure(title, '', 'conversation not found or not confirmed');
            scheduleBadgeSync();
            return true;
        });
    }

    // Called from Rust when a notification's reply field or action button is
    // used. `action` is 'reply' (with text) or 'read'.
    function handleNotificationAction(id, title, action, text) {
        activeNotifications.delete(id);
        if (action === 'reply') return replyToConversation(title, text);
        if (action === 'read') return markConversationRead(title);
        return Promise.resolve(false);
    }

    // --- Recent conversations for the tray popover ---
    // The popover lists the most recent conversations, read from the sidebar
    // in its order (including ones where the user sent the last message).
    // Rust keeps the latest list, so it is only sent when it changes.
    function readRecentConversations() {
        const items = document.querySelectorAll('mws-conversation-list-item');
        const titleCounts = new Map();
        for (let i = 0; i < items.length; i++) {
            const title = getConversationTitle(items[i]);
            if (title) titleCounts.set(title, (titleCounts.get(title) || 0) + 1);
        }
        const list = [];
        for (let i = 0; i < items.length && list.length < RECENT_CONVERSATION_LIMIT; i++) {
            const el = items[i];
            const title = getConversationTitle(el);
            const snippet = getConversationSnippet(el);
            if (!title) continue;
            list.push({
                title: title,
                snippet: snippet,
                time: childText(el, 'mws-relative-timestamp, [data-e2e-conversation-timestamp]'),
                unread: isConversationItemUnread(el),
                canReply: titleCounts.get(title) === 1
            });
        }
        return list;
    }

    function syncRecentConversations() {
        const list = readRecentConversations();
        const json = JSON.stringify(list);
        if (json === lastRecentConversationsJson) return;
        lastRecentConversationsJson = json;
        send('set_recent_conversations', { conversations: list });
    }

    // Called from Rust when something is chosen in the popover: 'reply' (with
    // text), 'read', or 'open' (Rust has already shown the window). Reports
    // the outcome back so the popover can say whether it worked.
    function handlePopoverAction(title, action, text) {
        let done;
        if (action === 'reply') done = replyToConversation(title, text);
        else if (action === 'read') done = markConversationRead(title);
        else if (action === 'open') done = openConversationByTitle(title).then(function (row) { return !!row; });
        else done = Promise.resolve(false);
        return done.then(function (ok) { return !!ok; }, function () { return false; }).then(function (ok) {
            send('popover_action_done', { title: title, action: action, ok: ok });
            syncRecentConversations();
            return ok;
        });
    }

    // --- Background requestAnimationFrame ---
    // Keep requestAnimationFrame progressing at a low rate when hidden in the
    // tray so Angular / Google Messages flushes DOM and <title> updates when
    // messages are read or replied to on the phone.
    //
    // Timer-backed frames get their own negative ids (real frame ids are
    // positive), so cancelling can tell the two apart: a real frame id is never
    // passed to clearTimeout, where it could cancel an unrelated timer.
    function backgroundRequestAnimationFrame(cb) {
        if (document.hidden && window.__GM_BACKGROUND_KEEP_ALIVE !== false) {
            const id = nextBackgroundFrameId--;
            backgroundFrameTimers.set(id, window.setTimeout(function () {
                backgroundFrameTimers.delete(id);
                cb(performance.now());
            }, 50));
            return id;
        }
        return origRAF.call(window, cb);
    }

    function backgroundCancelAnimationFrame(id) {
        if (backgroundFrameTimers.has(id)) {
            window.clearTimeout(backgroundFrameTimers.get(id));
            backgroundFrameTimers.delete(id);
        } else if (typeof origCAF === 'function') {
            try { origCAF.call(window, id); } catch (_) {}
        }
    }

    function installBackgroundAnimationFrame() {
        if (typeof origRAF === 'function') {
            window.requestAnimationFrame = backgroundRequestAnimationFrame;
            window.cancelAnimationFrame = backgroundCancelAnimationFrame;
        }
    }

    // --- "Google Messages is open on another computer" protection ---
    function findUseHereButton() {
        if (!onMessagesHost()) return null;
        const buttons = document.querySelectorAll('button, [role="button"]');
        for (let i = 0; i < buttons.length; i++) {
            const btn = buttons[i];
            if (!isElementVisible(btn)) continue;
            const label = ((btn.textContent || '') + ' ' + (btn.getAttribute('aria-label') || '')).trim();
            if (/^use here$/i.test(btn.textContent ? btn.textContent.trim() : '') || /\buse here\b/i.test(label)) {
                // Confirm an ancestor container mentions another computer/window/tab
                let cur = btn.parentElement;
                for (let depth = 0; cur && depth < 6; depth++, cur = cur.parentElement) {
                    const txt = cur.textContent || '';
                    if (txt.length < 600 && /another (computer|window|tab|browser)|open (on|in) another/i.test(txt)) {
                        return btn;
                    }
                }
            }
        }
        return null;
    }

    function detectOtherSession() {
        return !!findUseHereButton();
    }

    function syncSessionYielded(yielded) {
        if (yielded === localSessionYielded) return;
        localSessionYielded = yielded;
        send('set_session_yielded', { yielded: yielded });
        if (!yielded) {
            const modal = document.getElementById(TAKEOVER_MODAL_ID);
            if (modal) modal.remove();
        }
    }

    function showTakeoverPrompt() {
        if (document.getElementById(TAKEOVER_MODAL_ID)) return;
        if (!findUseHereButton()) return;

        const overlay = styledElement('div',
            'position:fixed;inset:0;z-index:2147483647;background:rgba(0,0,0,.55);' +
            'display:flex;align-items:center;justify-content:center;font-family:inherit;');
        overlay.id = TAKEOVER_MODAL_ID;

        const card = styledElement('div',
            'background:#1e1f20;color:#e3e3e3;border:1px solid #3c4043;border-radius:12px;' +
            'max-width:380px;width:90%;padding:20px 22px;box-shadow:0 12px 32px rgba(0,0,0,.45);');

        const title = styledElement('div',
            'font-size:16px;font-weight:600;margin-bottom:8px;',
            'Move Google Messages here?');

        const desc = styledElement('div',
            'font-size:13px;line-height:1.45;color:#9aa0a6;margin-bottom:18px;',
            'Google Messages is open on another computer. Move the active session to this app, or choose No to let this app sleep in the background.');

        const actions = styledElement('div', 'display:flex;justify-content:flex-end;gap:10px;');

        const noBtn = styledElement('button',
            'padding:8px 14px;border-radius:18px;border:1px solid #5f6368;background:transparent;' +
            'color:#e3e3e3;font:inherit;font-size:13px;cursor:pointer;',
            'No, let app sleep');
        noBtn.type = 'button';

        const yesBtn = styledElement('button',
            'padding:8px 16px;border-radius:18px;border:none;background:#8ab4f8;' +
            'color:#131314;font:inherit;font-size:13px;font-weight:600;cursor:pointer;',
            'Move session here');
        yesBtn.type = 'button';

        function decline() {
            overlay.remove();
            syncSessionYielded(true);
        }

        noBtn.addEventListener('click', decline);
        yesBtn.addEventListener('click', function () {
            overlay.remove();
            const btn = findUseHereButton();
            if (btn) btn.click();
            syncSessionYielded(false);
        });
        overlay.addEventListener('click', function (e) {
            if (e.target === overlay) decline();
        });

        actions.appendChild(noBtn);
        actions.appendChild(yesBtn);
        card.appendChild(title);
        card.appendChild(desc);
        card.appendChild(actions);
        overlay.appendChild(card);
        document.body.appendChild(overlay);
    }

    function checkSessionState() {
        if (!onMessagesHost()) return;
        const other = detectOtherSession();
        if (other) {
            syncSessionYielded(true);
        } else if (localSessionYielded) {
            const hasMainUi = !!document.querySelector('mws-conversations-list, mw-main-nav, [data-e2e-conversation-list]');
            if (hasMainUi) {
                syncSessionYielded(false);
            }
        }
    }

    // --- Background Heartbeat & Connection Watchdog ---
    function isUserBusy() {
        if (location.pathname.indexOf('/calls') !== -1) return true;
        if (document.querySelector('button[aria-label*="End call" i], button[aria-label*="Leave call" i], [data-e2e-end-call-button]')) {
            return true;
        }
        const active = document.activeElement;
        if (active && (active.tagName === 'TEXTAREA' || active.tagName === 'INPUT' || active.isContentEditable)) {
            const val = (active.value || active.textContent || '').trim();
            if (val.length > 0) return true;
        }
        const compose = document.querySelector('mw-message-compose textarea, [data-e2e-message-input-box], textarea[placeholder*="message" i]');
        if (compose && ((compose.value || compose.textContent || '').trim().length > 0)) {
            return true;
        }
        return false;
    }

    function detectConnectionProblem() {
        if (navigator.onLine === false) return 'offline';
        const banners = document.querySelectorAll(
            'mws-connection-banner, mw-connection-banner, [data-e2e-connection-error], [role="alert"], [role="status"]'
        );
        for (let i = 0; i < banners.length; i++) {
            const el = banners[i];
            if (!isElementVisible(el)) continue;
            if (el.closest && el.closest('mws-message-wrapper, mws-conversation-list-item')) continue;
            const txt = (el.textContent || '').trim();
            if (txt.length > 0 && txt.length < 240 &&
                /trying to reach your phone|unable to reach your phone|couldn['’]t connect|no internet|waiting for network|reconnecting/i.test(txt)) {
                return 'stalled-connection';
            }
        }
        return null;
    }

    function reportHeartbeat(healthy, reason) {
        send('report_heartbeat', { healthy: healthy, reason: reason });
    }

    // Called from the Rust background listener every 30s.
    function heartbeat(wokeFromSleep) {
        if (!onMessagesHost()) {
            reportHeartbeat(true, 'non-messages-host');
            return;
        }
        checkSessionState();
        if (localSessionYielded) {
            reportHeartbeat(true, 'other-session');
            return;
        }
        syncBadge();
        if (isUserBusy()) {
            reportHeartbeat(true, 'user-busy');
            return;
        }
        const problem = detectConnectionProblem();
        if (problem) {
            reportHeartbeat(false, problem);
        } else if (wokeFromSleep) {
            // Prod a lightweight fetch to verify the session socket/network after sleep
            fetch(location.origin + '/web/', { method: 'HEAD', cache: 'no-store' })
                .then(function () { reportHeartbeat(true, null); })
                .catch(function () { reportHeartbeat(false, 'wake-fetch-failed'); });
        } else {
            reportHeartbeat(true, null);
        }
    }

    function reloadIfStalledAfterReconnect() {
        if (!onMessagesHost()) return;
        setTimeout(function () {
            if (window.__GM_BACKGROUND_KEEP_ALIVE === false || localSessionYielded || isUserBusy()) return;
            if (detectConnectionProblem() === 'stalled-connection') {
                window.location.reload();
            }
        }, 6000);
    }

    // --- Startup wiring ---
    // Single body observer for the cheap per-mutation checks.
    // placeGear is cheap enough to run on every mutation. The session check
    // scans every button and reads layout, so it runs at most once per
    // SESSION_CHECK_DELAY_MS, after the burst of mutations settles.
    function scheduleSessionCheck() {
        if (sessionCheckTimer) return;
        sessionCheckTimer = setTimeout(function () {
            sessionCheckTimer = null;
            checkSessionState();
        }, SESSION_CHECK_DELAY_MS);
    }

    function onBodyMutation() {
        placeGear();
        scheduleSessionCheck();
    }

    function onReady() {
        injectDesktopStyles();
        observeTitle();
        placeGear();
        checkSessionState();
        bodyObserver.observe(document.body, { childList: true, subtree: true });
    }

    // =====================================================================
    // Startup: all frames
    // =====================================================================
    const MESSAGES_HOST = 'messages.google.com';

    disablePasskeys();

    // The remaining hooks (unread badge, settings gear, link routing,
    // Notification wrapper) only belong in the top-level frame.
    if (window !== window.top) {
        return;
    }

    // =====================================================================
    // Constants & state (top-level frame only)
    // =====================================================================
    const APP_TITLE = 'Google Messages';
    const GEAR_ID = 'gm-app-settings';
    const TAKEOVER_MODAL_ID = 'gm-takeover-modal';
    const GEAR_BASE_CSS =
        'width:34px !important;height:34px !important;min-width:34px !important;min-height:34px !important;' +
        'padding:0 !important;border:none !important;border-radius:50% !important;' +
        'background:rgba(128,128,128,.2) !important;color:inherit !important;' +
        'display:inline-flex !important;align-items:center !important;justify-content:center !important;' +
        'cursor:pointer !important;opacity:.82;box-sizing:border-box !important;flex-shrink:0 !important;' +
        'transition:opacity .15s ease,background-color .15s ease;';
    const GEAR_ICON_PATH =
        'M19.14 12.94c.04-.3.06-.61.06-.94 0-.32-.02-.64-.07-.94l2.03-1.58a.49.49 0 0 0 .12-.61l-1.92-3.32a.49.49 0 0 0-.59-.22l-2.39.96c-.5-.38-1.03-.7-1.62-.94l-.36-2.54a.484.484 0 0 0-.48-.41h-3.84c-.24 0-.43.17-.47.41l-.36 2.54c-.59.24-1.13.57-1.62.94l-2.39-.96a.49.49 0 0 0-.59.22L2.74 8.87c-.12.21-.08.47.12.61l2.03 1.58c-.05.3-.09.63-.09.94s.02.64.07.94l-2.03 1.58a.49.49 0 0 0-.12.61l1.92 3.32c.12.22.37.29.59.22l2.39-.96c.5.38 1.03.7 1.62.94l.36 2.54c.05.24.24.41.48.41h3.84c.24 0 .44-.17.47-.41l.36-2.54c.59-.24 1.13-.56 1.62-.94l2.39.96c.22.08.47 0 .59-.22l1.92-3.32c.12-.22.07-.47-.12-.61l-2.01-1.58zM12 15.6c-1.98 0-3.6-1.62-3.6-3.6s1.62-3.6 3.6-3.6 3.6 1.62 3.6 3.6-1.62 3.6-3.6 3.6z';
    const NEW_CHAT_SELECTORS = [
        'a[href*="/web/conversations/new"]',
        'mws-fab a',
        'mws-fab button',
        '[data-e2e-start-button]',
        'button[aria-label*="Start chat" i]',
        'a[aria-label*="Start chat" i]',
        '.start-chat button',
        '.fab-label'
    ];
    const SEARCH_INPUT_SELECTORS = [
        'input[placeholder*="Search" i]',
        'input[aria-label*="Search" i]',
        'mws-search-box input',
        'input[type="search"]'
    ];
    const COMPOSE_BOX_SELECTORS = [
        'mws-message-compose textarea',
        'mw-message-compose textarea',
        '[data-e2e-message-input-box]',
        'textarea[placeholder*="message" i]'
    ];
    const SEND_BUTTON_SELECTORS = [
        '[data-e2e-send-text-button]',
        'mws-message-send-button button',
        'button[aria-label*="Send" i]'
    ];
    // How long each step of a notification action may take before giving up.
    const ACTION_TIMEOUT_MS = 3000;
    // How long each way of sending gets before the next one is tried.
    const SEND_STEP_TIMEOUT_MS = 1500;
    const SEARCH_BUTTON_SELECTORS = [
        'button[aria-label*="Search" i]',
        '[data-e2e-search-button]',
        'mws-search-box button'
    ];

    const isMac = /Mac|iPod|iPhone|iPad/.test(navigator.platform || '');
    const origOpen = window.open;
    const origRAF = window.requestAnimationFrame;
    const origCAF = window.cancelAnimationFrame;

    // Unread / notification tracking
    let lastUnreadCount = null;
    let lastTitleRawCount = 0;
    let sawDomUnread = false;
    let notifClosedByAppSinceIncrease = false;
    let lastWebNotifAt = 0;
    let pendingFallbackTimer = null;
    let badgeSyncTimer = null;
    const activeNotifications = new Map();
    // Tray popover: how many conversations it lists, and the last list sent.
    const RECENT_CONVERSATION_LIMIT = 5;
    let lastRecentConversationsJson = null;
    // How long a new notification is exempt from read-conversation pruning.
    const PRUNE_GRACE_MS = 5000;

    // Timer-backed animation frames requested while hidden: frame id -> timer id.
    const backgroundFrameTimers = new Map();
    let nextBackgroundFrameId = -1;

    // Session-takeover tracking
    let localSessionYielded = false;
    let sessionCheckTimer = null;
    const SESSION_CHECK_DELAY_MS = 150;

    // Observers
    let observedTitleEl = null;
    const titleObserver = new MutationObserver(syncBadge);
    const convObserver = new MutationObserver(scheduleBadgeSync);
    const bodyObserver = new MutationObserver(onBodyMutation);

    // =====================================================================
    // Startup: top-level frame
    // =====================================================================
    // Live settings mirrored from Rust (set_settings re-evals these).
    window.__GM_SHOW_NOTIFICATIONS = __GM_NOTIFICATIONS_DEFAULT__;
    window.__GM_BACKGROUND_KEEP_ALIVE = __GM_KEEP_ALIVE_DEFAULT__;

    // Entry points called from Rust via eval.
    window.__gmNativeNotificationClicked = nativeNotificationClicked;
    window.__gmNotificationAction = handleNotificationAction;
    window.__gmPopoverAction = handlePopoverAction;
    window.__GM_SHOW_TAKEOVER_PROMPT = showTakeoverPrompt;
    window.__gmHeartbeat = heartbeat;

    // Browser API overrides.
    window.open = routedWindowOpen;
    installNotificationBridge();
    installBackgroundAnimationFrame();

    // Event listeners. Link clicks and keys use the capture phase so we beat
    // the app's own handlers.
    document.addEventListener('dragstart', preventGhostDrag, true);
    document.addEventListener('click', handleLinkClick, true);
    document.addEventListener('auxclick', handleLinkClick, true);
    document.addEventListener('keydown', handleKeydown, true);
    window.addEventListener('focus', syncBadgeOnFocus);
    window.addEventListener('online', reloadIfStalledAfterReconnect);
    setInterval(syncBadge, 2500);

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', onReady);
    } else {
        onReady();
    }
})();
