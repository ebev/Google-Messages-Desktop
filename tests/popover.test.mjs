// Tests for the tray popover page (frontend/popover.html), loaded into jsdom
// with a fake Tauri IPC object that answers get_popover_state and records
// every other command.
//
// Run with: npm test

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { JSDOM } from 'jsdom';

const HTML = readFileSync(new URL('../frontend/popover.html', import.meta.url), 'utf8');

const CONVERSATIONS = [
    { title: 'Alice', snippet: 'lunch?', time: '2 min', unread: true, canReply: true },
    { title: 'Bob', snippet: 'yo', time: 'Mon', unread: false, canReply: true },
];

async function loadPopover({ conversations = CONVERSATIONS, quickAction = '👍', connection = 'connected', os } = {}) {
    const calls = [];
    const dom = new JSDOM(HTML, {
        runScripts: 'dangerously',
        pretendToBeVisual: true,
        beforeParse(window) {
            window.matchMedia = () => ({ matches: false, addEventListener() {} });
            if (os) window.__GM_POPOVER_OS = os;
            window.__TAURI_INTERNALS__ = {
                invoke(cmd, args) {
                    calls.push({ cmd, args: args === undefined ? null : JSON.parse(JSON.stringify(args)) });
                    if (cmd === 'get_popover_state') {
                        return Promise.resolve({ conversations, quickAction, connection, theme: null });
                    }
                    return Promise.resolve(true);
                },
            };
        },
    });
    const { window } = dom;
    await new Promise((resolve) => window.addEventListener('load', resolve));
    await new Promise((resolve) => setTimeout(resolve, 0));
    const document = window.document;
    return {
        window,
        document,
        calls,
        sent(cmd) {
            return calls.filter((c) => c.cmd === cmd).map((c) => c.args);
        },
        cards() {
            return [...document.querySelectorAll('.conv')];
        },
        input() {
            return document.querySelector('.reply input');
        },
        type(text) {
            const input = document.querySelector('.reply input');
            input.value = text;
            input.dispatchEvent(new window.Event('input', { bubbles: true }));
        },
        key(key, target) {
            (target || document.activeElement || document.body).dispatchEvent(
                new window.KeyboardEvent('keydown', { key, bubbles: true, cancelable: true }));
        },
        close() {
            window.close();
        },
    };
}

test('lists the conversations with the first one selected and ready to reply', async (t) => {
    const p = await loadPopover();
    t.after(p.close);
    const cards = p.cards();
    assert.equal(cards.length, 2);
    assert.equal(cards[0].querySelector('.name').textContent, 'Alice');
    assert.ok(cards[0].classList.contains('selected'));
    assert.ok(cards[0].classList.contains('unread'));
    assert.ok(cards[0].querySelector('.reply input'), 'selected card has the reply box');
    assert.equal(cards[1].querySelector('.reply'), null);
    assert.equal(p.document.activeElement, p.input(), 'reply box has focus');
});

test('Enter sends the typed reply to the selected conversation', async (t) => {
    const p = await loadPopover();
    t.after(p.close);
    p.type('  sure  ');
    p.key('Enter', p.input());
    assert.deepEqual(p.sent('popover_action'), [{ title: 'Alice', action: 'reply', text: 'sure' }]);
    assert.ok(p.input().disabled, 'locked while sending');

    p.window.__gmPopoverActionDone('Alice', 'reply', true);
    assert.equal(p.input().value, '', 'sent text is cleared');
    assert.equal(p.document.getElementById('status').textContent, 'Sent to Alice');
});

test('a failed reply keeps the text', async (t) => {
    const p = await loadPopover();
    t.after(p.close);
    p.type('running late');
    p.key('Enter', p.input());
    p.window.__gmPopoverActionDone('Alice', 'reply', false);
    assert.equal(p.input().value, 'running late');
    assert.equal(p.document.getElementById('status').className, 'err');
});

test('clicking another conversation moves the reply box and keeps each draft', async (t) => {
    const p = await loadPopover();
    t.after(p.close);
    p.type('draft for alice');
    p.cards()[1].dispatchEvent(new p.window.MouseEvent('click', { bubbles: true }));
    assert.ok(p.cards()[1].classList.contains('selected'));
    assert.equal(p.input().value, '');
    p.key('ArrowUp', p.input());
    assert.ok(p.cards()[0].classList.contains('selected'));
    assert.equal(p.input().value, 'draft for alice');
});

test('the quick action and mark as read act on the selected conversation; double-click opens it', async (t) => {
    const p = await loadPopover();
    t.after(p.close);
    const labels = () => [...p.document.querySelectorAll('.reply-actions button')].map((b) => b.textContent);
    const button = (label) => [...p.document.querySelectorAll('.reply-actions button')]
        .find((b) => b.textContent === label);
    assert.deepEqual(labels(), ['👍', 'Mark as read'], 'no Open button');
    button('👍').click();
    assert.deepEqual(p.sent('popover_action').at(-1), { title: 'Alice', action: 'reply', text: '👍' });
    p.window.__gmPopoverActionDone('Alice', 'reply', true);
    button('Mark as read').click();
    assert.deepEqual(p.sent('popover_action').at(-1), { title: 'Alice', action: 'read', text: '' });
    p.window.__gmPopoverActionDone('Alice', 'read', true);
    p.cards()[1].dispatchEvent(new p.window.MouseEvent('dblclick', { bubbles: true }));
    assert.deepEqual(p.sent('popover_action').at(-1), { title: 'Bob', action: 'open', text: '' });
});

test('the footer opens settings and quits', async (t) => {
    const p = await loadPopover();
    t.after(p.close);
    p.document.getElementById('settingsBtn').click();
    p.document.getElementById('quitBtn').click();
    p.document.getElementById('openApp').click();
    assert.deepEqual(p.sent('popover_action').map((a) => a.action), ['settings', 'quit', 'show']);
});

test('no reply box is usable when two conversations share the name', async (t) => {
    const p = await loadPopover({
        conversations: [{ title: 'Sam', snippet: 'hey', time: '', unread: false, canReply: false }],
    });
    t.after(p.close);
    assert.ok(p.input().disabled);
    assert.match(p.document.querySelector('.note').textContent, /More than one conversation/);
});

test('updates from the page replace the list and Escape hides the popover', async (t) => {
    const p = await loadPopover();
    t.after(p.close);
    p.window.__gmPopoverUpdate([CONVERSATIONS[1]]);
    assert.deepEqual(p.cards().map((c) => c.querySelector('.name').textContent), ['Bob']);
    p.window.__gmPopoverUpdate([]);
    assert.match(p.document.querySelector('.empty').textContent, /No recent messages/);
    p.key('Escape', p.document.body);
    assert.equal(p.sent('hide_popover').length, 1);
});

// --- Connection status ---

function statusText(p) {
    return p.document.getElementById('status').textContent;
}

function useHereButton(p) {
    return p.document.getElementById('useHere');
}

test('the status bar says when Messages is connected', async (t) => {
    const p = await loadPopover();
    t.after(p.close);
    assert.equal(statusText(p), 'Connected');
    assert.equal(p.document.getElementById('statusBar').dataset.conn, 'connected');
    assert.ok(useHereButton(p).hidden);
});

test('an action outcome shows for a while, then the connection comes back', async (t) => {
    const p = await loadPopover();
    t.after(p.close);
    p.type('hi');
    p.key('Enter', p.input());
    p.window.__gmPopoverActionDone('Alice', 'reply', true);
    assert.equal(statusText(p), 'Sent to Alice');
    await new Promise((resolve) => setTimeout(resolve, 4100));
    assert.equal(statusText(p), 'Connected');
});

test('when open on another computer, "Use here" moves the session and replies wait for it', async (t) => {
    const p = await loadPopover({ connection: 'elsewhere' });
    t.after(p.close);
    assert.equal(statusText(p), 'Not connected');
    assert.ok(!useHereButton(p).hidden);
    assert.ok(p.input().disabled, 'no replying while the session is elsewhere');
    assert.match(p.document.querySelector('.note').textContent, /another computer/);

    useHereButton(p).click();
    assert.deepEqual(p.sent('popover_action').at(-1), { title: '', action: 'use_here', text: '' });
    assert.equal(statusText(p), 'Moving Messages here…');
    assert.ok(useHereButton(p).hidden);

    p.window.__gmPopoverActionDone('', 'use_here', true);
    p.window.__gmPopoverConnection('loading');
    assert.equal(statusText(p), 'Moving Messages here…', 'still waiting while the page reloads');
    p.window.__gmPopoverConnection('connected');
    assert.equal(statusText(p), 'Connected');
    assert.ok(useHereButton(p).hidden);
    assert.ok(!p.input().disabled);
});

test('"Use here" says so when the page could not move the session', async (t) => {
    const p = await loadPopover({ connection: 'elsewhere' });
    t.after(p.close);
    useHereButton(p).click();
    p.window.__gmPopoverActionDone('', 'use_here', false);
    assert.match(statusText(p), /Couldn’t move Messages here/);
    assert.equal(p.document.getElementById('status').className, 'err');
    assert.ok(!useHereButton(p).hidden, 'can try again');
});

test('other connection problems are shown without "Use here"', async (t) => {
    const p = await loadPopover({ connection: 'phone' });
    t.after(p.close);
    assert.equal(statusText(p), 'Can’t reach your phone');
    assert.ok(useHereButton(p).hidden);
    p.window.__gmPopoverConnection('offline');
    assert.equal(statusText(p), 'Offline');
    p.window.__gmPopoverConnection('');
    assert.equal(statusText(p), 'Connecting…');
});

// --- Platform looks ---

test('macOS gets the menu bar panel look with the status by the title', async (t) => {
    const p = await loadPopover({ os: 'macos' });
    t.after(p.close);
    assert.ok(p.document.documentElement.classList.contains('mac'));
    assert.equal(p.document.getElementById('statusBar').parentElement.tagName, 'HEADER');
    p.key(',', p.input());
    assert.equal(p.sent('popover_action').length, 0, 'plain keys are typed');
    p.document.activeElement.dispatchEvent(
        new p.window.KeyboardEvent('keydown', { key: ',', metaKey: true, bubbles: true, cancelable: true }));
    assert.deepEqual(p.sent('popover_action').at(-1), { title: '', action: 'settings', text: '' });
});

test('Windows gets the tray flyout look with the status in the footer', async (t) => {
    const p = await loadPopover({ os: 'windows' });
    t.after(p.close);
    assert.ok(p.document.documentElement.classList.contains('win'));
    assert.equal(p.document.getElementById('statusBar').parentElement.tagName, 'FOOTER');
});

test('avatars show initials, or a person icon for numbers', async (t) => {
    const p = await loadPopover({
        conversations: [
            { title: 'Jamie Moore', snippet: 'hi', time: '', unread: false, canReply: true },
            { title: '73981', snippet: 'code', time: '', unread: false, canReply: true },
        ],
    });
    t.after(p.close);
    const avatars = [...p.document.querySelectorAll('.avatar')];
    assert.equal(avatars[0].textContent, 'JM');
    assert.ok(avatars[1].querySelector('svg'));
});

test('avatars show the contact photo, and initials if it fails to load', async (t) => {
    const photo = 'data:image/png;base64,iVBORw0KGgo=';
    const p = await loadPopover({
        conversations: [
            { title: 'Jamie Moore', snippet: 'hi', time: '', unread: false, canReply: true, photo },
            { title: 'Alex Lin', snippet: 'yo', time: '', unread: false, canReply: true, photo: 'javascript:alert(1)' },
        ],
    });
    t.after(p.close);
    const avatars = [...p.document.querySelectorAll('.avatar')];
    const img = avatars[0].querySelector('img');
    assert.equal(img.getAttribute('src'), photo);
    assert.equal(avatars[1].querySelector('img'), null, 'only image data and https URLs are used');
    assert.equal(avatars[1].textContent, 'AL');
    img.dispatchEvent(new p.window.Event('error'));
    assert.equal(avatars[0].querySelector('img'), null);
    assert.equal(avatars[0].textContent, 'JM');
});

test('an empty list says it is loading until Messages has its conversations', async (t) => {
    const p = await loadPopover({ conversations: [], connection: 'loading' });
    t.after(p.close);
    const emptyText = () => p.document.querySelector('.empty').textContent;
    assert.match(emptyText(), /Loading conversations/);
    p.window.__gmPopoverConnection('signed-out');
    assert.match(emptyText(), /Not signed in/);
    p.window.__gmPopoverConnection('connected');
    assert.match(emptyText(), /No recent messages/);
    p.window.__gmPopoverUpdate(CONVERSATIONS);
    assert.equal(p.cards().length, 2);
});
