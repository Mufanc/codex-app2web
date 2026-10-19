import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { randomUUID } from 'node:crypto';
import { test } from 'node:test';
import vm from 'node:vm';

const script = readFileSync(new URL('./menu.js', import.meta.url), 'utf8');

function fixture() {
    const calls = [], events = [], timers = new Map();
    let time = 0, sequence = 0;
    const ipc = { invoke: (...args) => { calls.push(args); return Promise.resolve({ native: true }); } };
    const original = ipc.invoke;
    const context = vm.createContext({
        crypto: { randomUUID },
        performance: { now: () => time },
        __codexApp2WebMenuChanged: value => events.push(JSON.parse(value)),
        setTimeout: callback => { const id = ++sequence; timers.set(id, callback); return id; },
        clearTimeout: id => timers.delete(id),
        electron: { ipcRenderer: ipc },
    });
    vm.runInContext(`(${script}).call(electron)`, context);
    return { ipc, original, calls, events, timers, context, proxy: context.__codexApp2WebMenu, advance: ms => { time += ms; } };
}

const channel = 'codex_desktop:show-context-menu';
const items = [{ id: 'parent', label: 'Parent', submenu: [
    { id: 'enabled', label: 'Enabled' }, { id: 'disabled', label: 'Disabled', enabled: false },
] }];

test('native calls pass through outside the one-second remote window', async () => {
    const f = fixture();
    assert.equal((await f.ipc.invoke(channel, items, { x: 20, y: 30 })).native, true);
    f.proxy.arm();
    await f.ipc.invoke('other-channel', 'untouched');
    f.advance(1000);
    await f.ipc.invoke(channel, items, { x: 20, y: 30 });
    assert.equal(f.events.length, 0);
    assert.equal(f.calls.length, 3);
    f.proxy.stop();
    assert.equal(f.ipc.invoke, f.original);
});

test('remote selection validates request ID, disabled items and recursive submenus', async () => {
    const f = fixture();
    f.proxy.arm();
    const result = f.ipc.invoke(channel, items, { x: 20, y: 30 });
    const id = f.events[0].id;
    f.proxy.respond('stale', 'enabled');
    f.proxy.respond(id, 'disabled');
    f.proxy.respond(id, 'unknown');
    assert.equal(f.events.length, 1);
    f.proxy.respond(id, 'enabled');
    assert.equal((await result).id, 'enabled');
    assert.equal(f.events.at(-1), null);
    assert.equal(f.timers.size, 0);
    assert.equal(f.calls.length, 0);
});

test('replacing a menu, cancellation, timeout and teardown always resolve pending requests', async () => {
    const f = fixture();
    f.proxy.arm();
    const first = f.ipc.invoke(channel, items, { x: 1, y: 2 });
    const second = f.ipc.invoke(channel, items, { x: 1, y: 2 });
    assert.equal((await first).id, undefined);
    f.proxy.cancel();
    assert.equal((await second).id, undefined);
    f.proxy.arm();
    const third = f.ipc.invoke(channel, items, { x: 1, y: 2 });
    [...f.timers.values()][0]();
    assert.equal((await third).id, undefined);
    f.proxy.arm();
    const fourth = f.ipc.invoke(channel, items, { x: 1, y: 2 });
    f.context.__codexApp2WebMenuChanged = () => { throw Error('binding lost'); };
    f.proxy.stop();
    assert.equal((await fourth).id, undefined);
    assert.equal(f.ipc.invoke, f.original);
});

test('failed menu publication falls back to the original native menu', async () => {
    const f = fixture();
    f.proxy.arm();
    f.context.__codexApp2WebMenuChanged = () => { throw Error('disconnected'); };
    assert.equal((await f.ipc.invoke(channel, items, { x: 1, y: 2 })).native, true);
    assert.equal(f.calls.length, 1);
    assert.equal(f.timers.size, 0);
});

test('a native menu after cooldown cancels the previous remote request', async () => {
    const f = fixture();
    f.proxy.arm(42);
    const remote = f.ipc.invoke(channel, items, { x: 1, y: 2 });
    assert.equal(f.events[0].owner, 42);
    f.advance(1000);
    assert.equal((await f.ipc.invoke(channel, items, { x: 1, y: 2 })).native, true);
    assert.equal((await remote).id, undefined);
    assert.equal(f.events.at(-1), null);
});
