function installMenuProxy() {
    const ipc = this.ipcRenderer;
    const previous = globalThis.__codexApp2WebMenu;

    if (previous?.ipc === ipc) return true;
    previous?.stop();

    const original = ipc.invoke;
    const epoch = crypto.randomUUID();
    const INTERCEPT_MS = 1000;
    const REQUEST_TIMEOUT_MS = 60_000;

    let deadline = 0;
    let sequence = 0;
    let owner = null;
    let pending = null;

    function publish(menu) {
        globalThis.__codexApp2WebMenuChanged(JSON.stringify(menu));
    }

    function finish(requestId, selected) {
        if (!pending || pending.id !== requestId) return;

        const request = pending;

        pending = null;
        clearTimeout(request.timeout);

        try {
            publish(null);
        } catch {
            // The CDP binding can disappear before teardown finishes.
        }

        request.resolve(selected ? { id: selected } : {});
    }

    function enabledItem(items, id) {
        return items.some(item => item.enabled !== false && (
            item.submenu ? enabledItem(item.submenu, id) : item.id === id && item.type !== 'separator'
        ));
    }

    function invoke(channel, ...args) {
        if (channel !== 'codex_desktop:show-context-menu' || performance.now() >= deadline) {
            if (channel === 'codex_desktop:show-context-menu' && pending) finish(pending.id, null);

            return Reflect.apply(original, this, [channel, ...args]);
        }

        const [items, position] = args;

        if (!Array.isArray(items) || !Number.isFinite(position?.x) || !Number.isFinite(position?.y)) {
            return Reflect.apply(original, this, [channel, ...args]);
        }

        if (pending) finish(pending.id, null);

        const id = `${epoch}:${++sequence}`;

        return new Promise(resolve => {
            pending = { id, items, resolve, timeout: setTimeout(() => finish(id, null), REQUEST_TIMEOUT_MS) };

            try {
                publish({ id, owner, x: position.x, y: position.y, items });
            } catch {
                clearTimeout(pending.timeout);
                pending = null;
                resolve(Reflect.apply(original, this, [channel, ...args]));
            }
        });
    }

    globalThis.__codexApp2WebMenu = {
        ipc,
        arm(controller) {
            owner = controller;
            deadline = performance.now() + INTERCEPT_MS;
        },
        respond(id, selected) {
            if (pending?.id === id && (!selected || enabledItem(pending.items, selected))) finish(id, selected);
        },
        cancel() {
            deadline = 0;
            if (pending) finish(pending.id, null);
        },
        stop() {
            this.cancel();
            if (ipc.invoke === invoke) ipc.invoke = original;
            delete globalThis.__codexApp2WebMenu;
        },
    };

    ipc.invoke = invoke;

    // shortcut: a local menu in this one-second window is also proxied until input provenance is available.
    return ipc.invoke === invoke;
}
