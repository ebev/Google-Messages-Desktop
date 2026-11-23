// Regression tests for the script injected into the Google Messages page
// (src-tauri/src/init_script.js). Each test loads the script into a fresh
// jsdom page that stands in for messages.google.com, with a fake Tauri IPC
// object that records every command the script sends to Rust.
//
// Run with: npm test

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { JSDOM } from 'jsdom';

const SCRIPT = readFileSync(new URL('../src-tauri/src/init_script.js', import.meta.url), 'utf8');

const PAGE = `<!DOCTYPE html><html><head><title>Messages</title></head><body>
  <div><a id="avatar" aria-label="Google Account: test" href="#">A</a></div>
  <mws-conversations-list>
    <mws-conversation-list-item id="alice" data-e2e-is-unread="false">
      <span data-e2e-conversation-name>Alice</span><mws-conversation-snippet>hello</mws-conversation-snippet>
    </mws-conversation-list-item>
    <mws-conversation-list-item id="bob" data-e2e-is-unread="false">
      <span data-e2e-conversation-name>Bob</span><mws-conversation-snippet>yo</mws-conversation-snippet>
    </mws-conversation-list-item>
    <mws-conversation-list-item id="carol" data-e2e-is-unread="false">
      <span data-e2e-conversation-name>Carol</span><mws-conversation-snippet>You: ok</mws-conversation-snippet>
    </mws-conversation-list-item>
  </mws-conversations-list>
  <a id="external" href="https://example.com/page" target="_blank">external</a>
  <a id="internal" href="https://messages.google.com/web/calls" target="_blank">calls</a>
</body></html>`;

// Loads the script into a new page. Options:
//   platform       navigator.platform ('MacIntel', 'Linux x86_64', 'Win32')
//   title          initial document title, e.g. '(2) Messages'
//   unread         ids of sidebar rows to mark unread before the script runs
//   notifications  value substituted for the "show notifications" setting
//   webNotification  a window.Notification class present before the script runs
//   nativeEnter    the app can press a real Return key (as on macOS)
function loadPage(options = {}) {
    const dom = new JSDOM(PAGE, {
        url: 'https://messages.google.com/web/conversations',
        runScripts: 'outside-only',
        pretendToBeVisual: true,
    });
    const { window } = dom;
    const calls = [];

    window.__TAURI_INTERNALS__ = {
        invoke(cmd, args) {
            // Copy through JSON so the recorded value is a plain object from this
            // realm rather than the page's (deepEqual compares prototypes).
            calls.push({ cmd, args: args === undefined ? null : JSON.parse(JSON.stringify(args)) });
            if (cmd === 'press_enter_key' && options.nativeEnter) {
                const target = window.document.activeElement || window.document.body;
                target.dispatchEvent(new window.KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
                return Promise.resolve(true);
            }
            return Promise.resolve(null);
        },
    };
    Object.defineProperty(window.navigator, 'platform', { value: options.platform || 'MacIntel' });
    // jsdom does no layout, so report every element as visible and on screen.
    window.Element.prototype.getBoundingClientRect = () => ({ width: 10, height: 10, top: 10, left: 10 });
    if (options.webNotification) window.Notification = options.webNotification;
    // jsdom has no service workers; the script only needs the prototype to patch.
    window.ServiceWorkerRegistration = function ServiceWorkerRegistration() {};
    if (options.title) window.document.title = options.title;
    for (const id of options.unread || []) setUnread(window, id, true);

    window.eval(
        SCRIPT
            .replace('__GM_NOTIFICATIONS_DEFAULT__', String(options.notifications !== false))
            .replace('__GM_KEEP_ALIVE_DEFAULT__', 'true')
    );

    return {
        window,
        document: window.document,
        calls,
        // Every recorded call to the given command, in order.
        sent(cmd) {
            return calls.filter((c) => c.cmd === cmd);
        },
        // Runs the script's own sync pass now, the way the Rust heartbeat does.
        sync() {
            window.__gmHeartbeat(false);
        },
        // Moves the page's clock forward without waiting.
        advanceClock(ms) {
            const realNow = window.Date.now;
            window.Date.now = () => realNow() + ms;
        },
        close() {
            window.close();
        },
    };
}

function setUnread(window, id, unread) {
    window.document.getElementById(id).setAttribute('data-e2e-is-unread', String(unread));
}

function count(page, cmd) {
    return page.sent(cmd).length;
}

function last(page, cmd) {
    const matching = page.sent(cmd);
    return matching.length ? matching[matching.length - 1].args : undefined;
}

function click(window, element, type = 'click') {
    const event = new window.MouseEvent(type, { bubbles: true, cancelable: true, button: type === 'auxclick' ? 1 : 0 });
    element.dispatchEvent(event);
    return event;
}

function pressKey(window, key, modifiers = {}) {
    const event = new window.KeyboardEvent('keydown', { key, bubbles: true, cancelable: true, ...modifiers });
    window.document.dispatchEvent(event);
    return event;
}

// --- Unread badge ---

test('reports the unread count from the title and sidebar on load', (t) => {
    const page = loadPage({ title: '(2) Messages', unread: ['alice', 'bob'] });
    t.after(page.close);
    assert.deepEqual(last(page, 'set_tray_badge'), { count: 2 });
});

test('reports zero unread without scanning when the title has no count', (t) => {
    const page = loadPage();
    t.after(page.close);
    assert.deepEqual(last(page, 'set_tray_badge'), { count: 0 });
});

test('uses the lower of the title count and the unread sidebar rows', (t) => {
    const page = loadPage({ title: '(3) Messages', unread: ['alice'] });
    t.after(page.close);
    assert.deepEqual(last(page, 'set_tray_badge'), { count: 1 });
});

test('clears a stale title count once the sidebar shows everything read', (t) => {
    const page = loadPage({ title: '(1) Messages', unread: ['alice'] });
    t.after(page.close);
    setUnread(page.window, 'alice', false);
    page.sync();
    assert.deepEqual(last(page, 'set_tray_badge'), { count: 0 });
    assert.equal(page.document.title, 'Messages');
});

test('does not count a conversation whose latest message is the user\'s own reply', (t) => {
    const page = loadPage({ title: '(2) Messages', unread: ['alice', 'carol'] });
    t.after(page.close);
    // Carol's snippet is "You: ok", so only Alice's row counts as unread.
    assert.deepEqual(last(page, 'set_tray_badge'), { count: 1 });
});

// --- Notifications ---

test('sends web notifications to the native bridge, keyed by tag', (t) => {
    const page = loadPage({ title: '(1) Messages', unread: ['alice'] });
    t.after(page.close);
    new page.window.Notification('Alice', { body: 'hello', tag: 'conv-1' });
    new page.window.Notification('Bob', { body: 'yo' });
    assert.deepEqual(page.sent('send_native_notification').map((c) => c.args), [
        { id: 'conv-1', title: 'Alice', body: 'hello', kind: 'message', canReply: true },
        { id: 'gm-conv-Bob', title: 'Bob', body: 'yo', kind: 'message', canReply: true },
    ]);
});

test('sends nothing when notifications are turned off', (t) => {
    const page = loadPage({ notifications: false });
    t.after(page.close);
    const n = new page.window.Notification('Alice', { body: 'hello' });
    n.close();
    assert.equal(count(page, 'send_native_notification'), 0);
    assert.equal(count(page, 'close_native_notification'), 0);
});

test('closes the native notification when the web app closes its own', (t) => {
    const page = loadPage({ title: '(1) Messages', unread: ['alice'] });
    t.after(page.close);
    const n = new page.window.Notification('Alice', { body: 'hello', tag: 'conv-1' });
    n.close();
    assert.deepEqual(last(page, 'close_native_notification'), { id: 'conv-1' });
});

test('a native click runs the web app\'s click handlers exactly once', (t) => {
    const page = loadPage({ title: '(1) Messages', unread: ['alice'] });
    t.after(page.close);
    const n = new page.window.Notification('Alice', { body: 'hello', tag: 'conv-1' });
    let onclick = 0;
    let listener = 0;
    n.onclick = () => { onclick++; };
    n.addEventListener('click', () => { listener++; });
    page.window.__gmNativeNotificationClicked('conv-1');
    page.window.__gmNativeNotificationClicked('conv-1');
    page.window.__gmNativeNotificationClicked('unknown-id');
    assert.deepEqual([onclick, listener], [1, 1]);
});

test('keeps a new notification through the grace period, then prunes it once its conversation is read', (t) => {
    const page = loadPage({ title: '(2) Messages', unread: ['alice'] });
    t.after(page.close);
    // Bob's row is still marked read: the notification arrived before the sidebar updated.
    new page.window.Notification('Bob', { body: 'yo' });
    page.sync();
    assert.equal(count(page, 'close_native_notification'), 0, 'closed during the grace period');
    page.advanceClock(6000);
    page.sync();
    assert.deepEqual(last(page, 'close_native_notification'), { id: 'gm-conv-Bob' });
});

test('never prunes a notification whose title matches no sidebar conversation', (t) => {
    const page = loadPage({ title: '(2) Messages', unread: ['alice'] });
    t.after(page.close);
    new page.window.Notification('Someone in a group', { body: 'hi' });
    page.advanceClock(60000);
    page.sync();
    assert.equal(count(page, 'close_native_notification'), 0);
});

test('keeps a notification whose conversation is still unread', (t) => {
    const page = loadPage({ title: '(1) Messages', unread: ['alice'] });
    t.after(page.close);
    new page.window.Notification('Alice', { body: 'hello' });
    page.advanceClock(60000);
    page.sync();
    assert.equal(count(page, 'close_native_notification'), 0);
});

test('clears native notifications once when unread drops to zero, not on every sync', (t) => {
    const page = loadPage({ title: '(1) Messages', unread: ['alice'] });
    t.after(page.close);
    new page.window.Notification('Alice', { body: 'hello' });
    const before = count(page, 'clear_native_notifications');
    setUnread(page.window, 'alice', false);
    page.document.title = 'Messages';
    page.sync();
    page.sync();
    page.sync();
    assert.equal(count(page, 'clear_native_notifications') - before, 1);
});

test('lists and filters active notifications for the service worker API', async (t) => {
    const page = loadPage({ title: '(1) Messages', unread: ['alice'] });
    t.after(page.close);
    const registration = page.window.ServiceWorkerRegistration;
    await registration.prototype.showNotification.call({}, 'Alice', { body: 'hello', tag: 'sw-1' });
    assert.deepEqual(last(page, 'send_native_notification').id, 'sw-1');
    assert.equal((await registration.prototype.getNotifications.call({}, { tag: 'sw-1' })).length, 1);
    assert.equal((await registration.prototype.getNotifications.call({}, { tag: 'other' })).length, 0);
});

test('on Windows, notifications go through the native bridge like other platforms', (t) => {
    // A page Notification class must not be used, even where one exists.
    let constructed = 0;
    class PageNotification {
        static permission = 'default';
        static requestPermission() { return Promise.resolve('denied'); }
        constructor() { constructed += 1; }
    }
    const page = loadPage({ platform: 'Win32', webNotification: PageNotification, title: '(1) Messages', unread: ['alice'] });
    t.after(page.close);
    assert.equal(page.window.Notification.permission, 'granted');
    const n = new page.window.Notification('Alice', { body: 'hello', tag: 'conv-1' });
    assert.equal(constructed, 0);
    assert.equal(last(page, 'send_native_notification').id, 'conv-1');
    n.close();
    assert.equal(last(page, 'close_native_notification').id, 'conv-1');
});

// --- Background animation frames ---

test('cancelAnimationFrame never cancels an unrelated timer', async (t) => {
    const page = loadPage();
    t.after(page.close);
    const { window } = page;
    Object.defineProperty(window.document, 'hidden', { configurable: true, get: () => true });

    const fired = [];
    const cancelled = window.requestAnimationFrame(() => fired.push('cancelled frame'));
    const kept = window.requestAnimationFrame(() => fired.push('kept frame'));
    assert.ok(cancelled < 0 && kept < 0, 'background frames use negative ids');

    const timer = window.setTimeout(() => fired.push('timer'), 20);
    window.cancelAnimationFrame(timer); // a real frame id that happens to equal a timer id
    window.cancelAnimationFrame(cancelled);

    await new Promise((resolve) => setTimeout(resolve, 150));
    assert.deepEqual(fired.sort(), ['kept frame', 'timer']);
});

// --- Session takeover ---

function addUseHereBanner(document) {
    const banner = document.createElement('div');
    banner.id = 'banner';
    banner.innerHTML = '<p>Google Messages is open on another computer</p><button>Use here</button>';
    document.body.appendChild(banner);
    return banner;
}

test('yields the session when "open on another computer" appears, and resumes when it clears', (t) => {
    const page = loadPage();
    t.after(page.close);
    const banner = addUseHereBanner(page.document);
    page.sync();
    assert.deepEqual(last(page, 'set_session_yielded'), { yielded: true });
    assert.deepEqual(last(page, 'report_heartbeat'), { healthy: true, reason: 'other-session' });

    banner.remove();
    page.sync();
    assert.deepEqual(last(page, 'set_session_yielded'), { yielded: false });
});

test('the takeover prompt clicks "Use here" when the user accepts', (t) => {
    const page = loadPage();
    t.after(page.close);
    const banner = addUseHereBanner(page.document);
    let useHereClicks = 0;
    banner.querySelector('button').addEventListener('click', () => { useHereClicks++; });
    page.sync();

    page.window.__GM_SHOW_TAKEOVER_PROMPT();
    const modal = page.document.getElementById('gm-takeover-modal');
    assert.ok(modal, 'prompt is shown');
    const accept = [...modal.querySelectorAll('button')].find((b) => b.textContent === 'Move session here');
    click(page.window, accept);
    assert.equal(useHereClicks, 1);
    assert.equal(page.document.getElementById('gm-takeover-modal'), null);
});

test('an ordinary "Use here" button with no takeover text is ignored', (t) => {
    const page = loadPage();
    t.after(page.close);
    page.document.body.insertAdjacentHTML('beforeend', '<div><button>Use here</button></div>');
    page.sync();
    assert.equal(count(page, 'set_session_yielded'), 0);
});

// --- Heartbeat ---

test('reports a healthy heartbeat on an idle page', (t) => {
    const page = loadPage();
    t.after(page.close);
    page.sync();
    assert.deepEqual(last(page, 'report_heartbeat'), { healthy: true, reason: null });
});

test('reports a stalled connection when the reconnect banner is showing', (t) => {
    const page = loadPage();
    t.after(page.close);
    page.document.body.insertAdjacentHTML('beforeend', '<div role="alert">Trying to reach your phone…</div>');
    page.sync();
    assert.deepEqual(last(page, 'report_heartbeat'), { healthy: false, reason: 'stalled-connection' });
});

test('never asks for a reload while the user is typing a message', (t) => {
    const page = loadPage();
    t.after(page.close);
    page.document.body.insertAdjacentHTML('beforeend',
        '<div role="alert">Trying to reach your phone…</div><mw-message-compose><textarea>draft</textarea></mw-message-compose>');
    page.sync();
    assert.deepEqual(last(page, 'report_heartbeat'), { healthy: true, reason: 'user-busy' });
});

// --- Link routing ---

test('opens external links in the OS browser instead of the app window', (t) => {
    const page = loadPage();
    t.after(page.close);
    const event = click(page.window, page.document.getElementById('external'));
    assert.equal(event.defaultPrevented, true);
    assert.deepEqual(last(page, 'open_external'), { url: 'https://example.com/page' });
});

test('window.open sends external URLs to the OS browser', (t) => {
    const page = loadPage();
    t.after(page.close);
    assert.equal(page.window.open('https://example.org/x'), null);
    assert.deepEqual(last(page, 'open_external'), { url: 'https://example.org/x' });
});

test('ignores non-http links', (t) => {
    const page = loadPage();
    t.after(page.close);
    page.document.body.insertAdjacentHTML('beforeend', '<a id="mail" href="mailto:a@example.com" target="_blank">mail</a>');
    click(page.window, page.document.getElementById('mail'));
    assert.equal(count(page, 'open_external'), 0);
});

// --- Keyboard shortcuts ---

test('on Windows and Linux, Ctrl shortcuts open settings and zoom', (t) => {
    const page = loadPage({ platform: 'Win32' });
    t.after(page.close);
    pressKey(page.window, ',', { ctrlKey: true });
    pressKey(page.window, '=', { ctrlKey: true });
    pressKey(page.window, '-', { ctrlKey: true });
    pressKey(page.window, '0', { ctrlKey: true });
    assert.equal(count(page, 'open_settings'), 1);
    assert.deepEqual(page.sent('adjust_zoom').map((c) => c.args.action), ['in', 'out', 'reset']);
});

test('on macOS, settings and zoom shortcuts are left to the native menu', (t) => {
    const page = loadPage({ platform: 'MacIntel' });
    t.after(page.close);
    pressKey(page.window, ',', { metaKey: true });
    pressKey(page.window, '=', { metaKey: true });
    assert.equal(count(page, 'open_settings'), 0);
    assert.equal(count(page, 'adjust_zoom'), 0);
});

// --- Page chrome ---

test('adds the settings gear next to the account avatar and opens settings on click', (t) => {
    const page = loadPage();
    t.after(page.close);
    const gear = page.document.getElementById('gm-app-settings');
    assert.ok(gear, 'gear exists');
    assert.equal(gear.nextElementSibling.id, 'avatar');
    click(page.window, gear);
    assert.equal(count(page, 'open_settings'), 1);
});

test('when signed out, the settings gear sits beside the Sign in button', async (t) => {
    const page = loadPage();
    t.after(page.close);
    const { document } = page;
    // Swap the signed-in corner for the signed-out one, as the welcome page has.
    document.getElementById('gm-app-settings').remove();
    document.getElementById('avatar').remove();
    const switcher = document.createElement('mw-ogbaccountswitcher');
    switcher.innerHTML = '<button id="signin" class="sign-in">Sign in</button>';
    document.body.prepend(switcher);
    await new Promise((resolve) => setTimeout(resolve, 50));
    const gear = document.getElementById('gm-app-settings');
    assert.ok(gear, 'gear exists');
    assert.equal(gear.nextElementSibling.id, 'signin');
});

test('reports passkeys as unavailable', async (t) => {
    const page = loadPage();
    t.after(page.close);
    assert.equal(page.window.PublicKeyCredential, undefined);
});

// --- Quick reply / mark as read from a notification ---

// Adds a message box and send button that behave like the real ones: sending
// records the text against whichever conversation is open and empties the box.
// Clicking a sidebar row "opens" it by marking it selected.
function addConversationUi(page, { sendWorks = true, enterSends = false, hasButton = true } = {}) {
    const { document, window } = page;
    const sentMessages = [];
    for (const row of document.querySelectorAll('mws-conversation-list-item')) {
        row.addEventListener('click', () => {
            for (const other of document.querySelectorAll('mws-conversation-list-item')) other.classList.remove('selected');
            row.classList.add('selected');
        });
    }
    document.body.insertAdjacentHTML('beforeend',
        '<mws-message-compose><textarea placeholder="Text message"></textarea>' +
        '<mws-message-send-button><button data-e2e-send-text-button>Send</button></mws-message-send-button></mws-message-compose>');
    const box = document.querySelector('mws-message-compose textarea');
    document.querySelector('[data-e2e-send-text-button]').addEventListener('click', () => {
        if (!sendWorks) return;
        const open = document.querySelector('mws-conversation-list-item.selected [data-e2e-conversation-name]');
        sentMessages.push({ to: open ? open.textContent : null, text: box.value });
        box.value = '';
    });
    const record = () => {
        const open = document.querySelector('mws-conversation-list-item.selected [data-e2e-conversation-name]');
        sentMessages.push({ to: open ? open.textContent : null, text: box.value });
        box.value = '';
    };
    box.addEventListener('keydown', (event) => {
        if (enterSends && event.key === 'Enter' && box.value) record();
    });
    if (!hasButton) document.querySelector('mws-message-send-button').remove();
    return { box, sentMessages, window };
}

test('a reply from a notification is sent to that conversation', async (t) => {
    const page = loadPage({ title: '(1) Messages', unread: ['bob'] });
    t.after(page.close);
    const ui = addConversationUi(page);
    const sent = await page.window.__gmNotificationAction('gm-conv-Bob', 'Bob', 'reply', '  on my way  ');
    assert.equal(sent, true);
    assert.deepEqual(ui.sentMessages, [{ to: 'Bob', text: 'on my way' }]);
    assert.equal(count(page, 'focus_main_window'), 0, 'window stays hidden on success');
});

test('a reply switches away from a different open conversation first', async (t) => {
    const page = loadPage({ title: '(1) Messages', unread: ['bob'] });
    t.after(page.close);
    const ui = addConversationUi(page);
    click(page.window, page.document.getElementById('alice'));
    await page.window.__gmNotificationAction('gm-conv-Bob', 'Bob', 'reply', 'hi');
    assert.deepEqual(ui.sentMessages, [{ to: 'Bob', text: 'hi' }]);
});

test('a reply is not sent when two conversations share the name', async (t) => {
    const page = loadPage({ title: '(1) Messages', unread: ['bob'] });
    t.after(page.close);
    page.document.getElementById('alice').querySelector('[data-e2e-conversation-name]').textContent = 'Bob';
    const ui = addConversationUi(page);
    const sent = await page.window.__gmNotificationAction('gm-conv-Bob', 'Bob', 'reply', 'hi');
    assert.equal(sent, false);
    assert.deepEqual(ui.sentMessages, []);
    assert.equal(ui.box.value, '');
    assert.equal(count(page, 'focus_main_window'), 1, 'window is shown so the user can reply by hand');
    assert.equal(last(page, 'send_native_notification').title, 'Reply not sent');
});

test('a reply is not sent when the conversation is not in the sidebar', async (t) => {
    const page = loadPage();
    t.after(page.close);
    const ui = addConversationUi(page);
    assert.equal(await page.window.__gmNotificationAction('x', 'Nobody', 'reply', 'hi'), false);
    assert.deepEqual(ui.sentMessages, []);
});

test('a reply never overwrites or sends a draft already in the message box', async (t) => {
    const page = loadPage({ title: '(1) Messages', unread: ['bob'] });
    t.after(page.close);
    const ui = addConversationUi(page);
    ui.box.value = 'half-written draft';
    assert.equal(await page.window.__gmNotificationAction('gm-conv-Bob', 'Bob', 'reply', 'hi'), false);
    assert.deepEqual(ui.sentMessages, []);
    assert.equal(ui.box.value, 'half-written draft');
});

test('if sending does not go through, the text stays as a draft and the window is shown', async (t) => {
    const page = loadPage({ title: '(1) Messages', unread: ['bob'] });
    t.after(page.close);
    const ui = addConversationUi(page, { sendWorks: false });
    assert.equal(await page.window.__gmNotificationAction('gm-conv-Bob', 'Bob', 'reply', 'hi'), false);
    assert.equal(ui.box.value, 'hi');
    assert.equal(count(page, 'focus_main_window'), 1);
});

test('a reply falls back to the Enter key when clicking Send does nothing', async (t) => {
    const page = loadPage({ title: '(1) Messages', unread: ['bob'] });
    t.after(page.close);
    const ui = addConversationUi(page, { sendWorks: false, enterSends: true });
    assert.equal(await page.window.__gmNotificationAction('gm-conv-Bob', 'Bob', 'reply', 'hi'), true);
    assert.deepEqual(ui.sentMessages, [{ to: 'Bob', text: 'hi' }]);
    assert.equal(count(page, 'focus_main_window'), 0);
});

test('a reply is sent with a real Return key press where the app supports it', async (t) => {
    const page = loadPage({ title: '(1) Messages', unread: ['bob'], nativeEnter: true });
    t.after(page.close);
    const ui = addConversationUi(page, { enterSends: true });
    assert.equal(await page.window.__gmNotificationAction('gm-conv-Bob', 'Bob', 'reply', 'hi'), true);
    assert.deepEqual(ui.sentMessages, [{ to: 'Bob', text: 'hi' }]);
    assert.equal(count(page, 'press_enter_key'), 1);
    assert.equal(count(page, 'focus_main_window'), 0);
});

test('a reply is sent with the Enter key when there is no Send button', async (t) => {
    const page = loadPage({ title: '(1) Messages', unread: ['bob'] });
    t.after(page.close);
    const ui = addConversationUi(page, { hasButton: false, enterSends: true });
    assert.equal(await page.window.__gmNotificationAction('gm-conv-Bob', 'Bob', 'reply', 'hi'), true);
    assert.deepEqual(ui.sentMessages, [{ to: 'Bob', text: 'hi' }]);
});

test('a reply is sent once when both the Send button and Enter work', async (t) => {
    const page = loadPage({ title: '(1) Messages', unread: ['bob'] });
    t.after(page.close);
    const ui = addConversationUi(page, { enterSends: true });
    assert.equal(await page.window.__gmNotificationAction('gm-conv-Bob', 'Bob', 'reply', 'hi'), true);
    assert.deepEqual(ui.sentMessages, [{ to: 'Bob', text: 'hi' }]);
});

test('mark as read opens the conversation without sending anything', async (t) => {
    const page = loadPage({ title: '(1) Messages', unread: ['bob'] });
    t.after(page.close);
    const ui = addConversationUi(page);
    assert.equal(await page.window.__gmNotificationAction('gm-conv-Bob', 'Bob', 'read', ''), true);
    assert.ok(page.document.getElementById('bob').classList.contains('selected'));
    assert.deepEqual(ui.sentMessages, []);
});

test('reply buttons are only offered when the title matches exactly one conversation', (t) => {
    const page = loadPage({ title: '(1) Messages', unread: ['bob'] });
    t.after(page.close);
    new page.window.Notification('Bob', { body: 'yo' });
    assert.equal(last(page, 'send_native_notification').canReply, true);
    new page.window.Notification('Someone in a group', { body: 'hi' });
    assert.equal(last(page, 'send_native_notification').canReply, false);
});

// --- Tray popover ---

test('the popover gets the recent conversations in sidebar order, including the user\'s own last messages', (t) => {
    const page = loadPage({ title: '(1) Messages', unread: ['bob'] });
    t.after(page.close);
    page.sync();
    assert.deepEqual(last(page, 'set_recent_conversations'), {
        conversations: [
            { title: 'Alice', snippet: 'hello', time: '', unread: false, canReply: true },
            { title: 'Bob', snippet: 'yo', time: '', unread: true, canReply: true },
            { title: 'Carol', snippet: 'You: ok', time: '', unread: false, canReply: true },
        ],
    });
});

test('the popover list is only sent again when it changes', (t) => {
    const page = loadPage();
    t.after(page.close);
    page.sync();
    const before = count(page, 'set_recent_conversations');
    assert.ok(before >= 1);
    page.sync();
    assert.equal(count(page, 'set_recent_conversations'), before, 'unchanged list is not resent');
    page.document.querySelector('#bob mws-conversation-snippet').textContent = 'You: sure';
    page.sync();
    assert.equal(count(page, 'set_recent_conversations'), before + 1);
    assert.deepEqual(last(page, 'set_recent_conversations').conversations.map((c) => c.snippet),
        ['hello', 'You: sure', 'You: ok']);
});

test('the popover does not offer a reply when two conversations share a name', (t) => {
    const page = loadPage();
    t.after(page.close);
    page.document.querySelector('#carol [data-e2e-conversation-name]').textContent = 'Alice';
    page.sync();
    const alice = last(page, 'set_recent_conversations').conversations.filter((c) => c.title === 'Alice');
    assert.equal(alice.length, 2);
    assert.ok(alice.every((c) => c.canReply === false));
});

test('a reply from the popover is sent and its outcome reported back', async (t) => {
    const page = loadPage();
    t.after(page.close);
    const ui = addConversationUi(page);
    const sent = await page.window.__gmPopoverAction('Bob', 'reply', 'be there soon');
    assert.equal(sent, true);
    assert.deepEqual(ui.sentMessages, [{ to: 'Bob', text: 'be there soon' }]);
    assert.deepEqual(last(page, 'popover_action_done'), { title: 'Bob', action: 'reply', ok: true });
});

test('opening a conversation from the popover selects it in the page', async (t) => {
    const page = loadPage();
    t.after(page.close);
    addConversationUi(page);
    const opened = await page.window.__gmPopoverAction('Alice', 'open', '');
    assert.equal(opened, true);
    assert.ok(page.document.getElementById('alice').classList.contains('selected'));
    assert.deepEqual(last(page, 'popover_action_done'), { title: 'Alice', action: 'open', ok: true });
});

test('a popover action for an unknown conversation reports failure', async (t) => {
    const page = loadPage();
    t.after(page.close);
    addConversationUi(page);
    const sent = await page.window.__gmPopoverAction('Nobody', 'reply', 'hello?');
    assert.equal(sent, false);
    assert.deepEqual(last(page, 'popover_action_done'), { title: 'Nobody', action: 'reply', ok: false });
});
