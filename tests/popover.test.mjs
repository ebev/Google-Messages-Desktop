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

async function loadPopover({ conversations = CONVERSATIONS, quickAction = '👍' } = {}) {
    const calls = [];
    const dom = new JSDOM(HTML, {
        runScripts: 'dangerously',
        pretendToBeVisual: true,
        beforeParse(window) {
            window.matchMedia = () => ({ matches: false, addEventListener() {} });
            window.__TAURI_INTERNALS__ = {
                invoke(cmd, args) {
                    calls.push({ cmd, args: args === undefined ? null : JSON.parse(JSON.stringify(args)) });
                    if (cmd === 'get_popover_state') {
                        return Promise.resolve({ conversations, quickAction, theme: null });
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

test('the quick action, mark as read and open buttons act on the selected conversation', async (t) => {
    const p = await loadPopover();
    t.after(p.close);
    const button = (label) => [...p.document.querySelectorAll('.reply-actions button')]
        .find((b) => b.textContent === label);
    button('👍').click();
    assert.deepEqual(p.sent('popover_action').at(-1), { title: 'Alice', action: 'reply', text: '👍' });
    p.window.__gmPopoverActionDone('Alice', 'reply', true);
    button('Mark as read').click();
    assert.deepEqual(p.sent('popover_action').at(-1), { title: 'Alice', action: 'read', text: '' });
    p.window.__gmPopoverActionDone('Alice', 'read', true);
    button('Open').click();
    assert.deepEqual(p.sent('popover_action').at(-1), { title: 'Alice', action: 'open', text: '' });
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
