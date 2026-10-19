(() => {
    const key = '__codexApp2WebCursor';
    globalThis[key]?.stop();

    let scheduled = 0;
    let previous;
    let observed;
    const resize = new ResizeObserver(schedule);
    const events = ['selectionchange', 'select', 'input', 'keyup', 'pointerup', 'focusin', 'focusout', 'scroll'];

    function number(style, property) {
        return Number.parseFloat(style[property]) || 0;
    }

    function activeElement() {
        let element = document.activeElement;

        while (element?.shadowRoot?.activeElement) element = element.shadowRoot.activeElement;

        return element;
    }

    function inputCaret(element) {
        if (element.selectionStart === null) return null;

        const position = element.selectionDirection === 'backward' ? element.selectionStart : element.selectionEnd;
        const style = getComputedStyle(element);
        const mirror = document.createElement('div');
        const marker = document.createElement('span');
        const isInput = element instanceof HTMLInputElement;
        const value = element.type === 'password' ? '•'.repeat(element.value.length) : element.value;

        for (const property of style) mirror.style.setProperty(property, style.getPropertyValue(property));

        Object.assign(mirror.style, {
            position: 'fixed', left: '0', top: '0', margin: '0', visibility: 'hidden',
            pointerEvents: 'none', transform: 'none', transition: 'none', animation: 'none',
            height: 'auto', minHeight: '0', maxHeight: 'none', overflow: 'hidden',
            boxSizing: 'border-box', width: `${element.clientWidth + number(style, 'borderLeftWidth') + number(style, 'borderRightWidth')}px`,
            whiteSpace: isInput || element.wrap === 'off' ? 'pre' : 'pre-wrap',
            overflowWrap: isInput ? 'normal' : 'break-word',
        });

        if (isInput) {
            mirror.style.lineHeight = `${element.clientHeight - number(style, 'paddingTop') - number(style, 'paddingBottom')}px`;
        }

        mirror.textContent = value.slice(0, position);
        marker.textContent = value.slice(position) || '\u200b';
        mirror.append(marker);
        document.documentElement.append(mirror);

        try {
            const range = document.createRange();

            range.setStart(marker.firstChild, 0);
            range.collapse(true);

            const caret = range.getClientRects()[0];
            const origin = mirror.getBoundingClientRect();
            const bounds = element.getBoundingClientRect();
            const scaleX = bounds.width / element.offsetWidth;
            const scaleY = bounds.height / element.offsetHeight;

            if (!caret || !Number.isFinite(scaleX) || !Number.isFinite(scaleY)) return null;

            return {
                x: bounds.left + (caret.left - origin.left - element.scrollLeft) * scaleX,
                y: bounds.top + (caret.bottom - origin.top - element.scrollTop) * scaleY,
            };
        } finally {
            mirror.remove();
        }
    }

    function editableCaret(element) {
        const root = element.getRootNode();
        const selection = root.getSelection?.() ?? document.getSelection();

        if (!selection?.focusNode || !element.contains(selection.focusNode)) return null;

        const range = document.createRange();
        range.setStart(selection.focusNode, selection.focusOffset);
        range.collapse(true);
        const rect = range.getClientRects()[0];

        if (rect?.height) return { x: rect.left, y: rect.bottom };

        // Empty editable blocks have no text rectangle; use their actual content inset.
        const block = selection.focusNode.nodeType === Node.ELEMENT_NODE
            ? selection.focusNode : selection.focusNode.parentElement;
        const child = block.childNodes[selection.focusOffset];
        const target = child instanceof HTMLElement ? child : block;
        const bounds = target.getBoundingClientRect();
        const style = getComputedStyle(target);
        const lineHeight = Number.parseFloat(style.lineHeight) || number(style, 'fontSize');

        return {
            x: bounds.left + number(style, 'borderLeftWidth') + number(style, 'paddingLeft'),
            y: bounds.top + number(style, 'borderTopWidth') + number(style, 'paddingTop') + lineHeight,
        };
    }

    function update() {
        scheduled = 0;

        const element = activeElement();

        if (element !== observed) {
            resize.disconnect();
            if (element) resize.observe(element);
            observed = element;
        }

        let point = null;

        if (element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement) point = inputCaret(element);
        else if (element?.isContentEditable) point = editableCaret(element);

        if (point && (!Number.isFinite(point.x) || !Number.isFinite(point.y))) point = null;

        const payload = JSON.stringify(point);

        if (payload !== previous) {
            previous = payload;
            globalThis.__codexApp2WebCaret(payload);
        }
    }

    function schedule() {
        if (!scheduled) scheduled = requestAnimationFrame(update);
    }

    for (const event of events) document.addEventListener(event, schedule, true);
    window.addEventListener('resize', schedule);
    window.visualViewport?.addEventListener('resize', schedule);
    window.visualViewport?.addEventListener('scroll', schedule);

    globalThis[key] = {
        stop() {
            cancelAnimationFrame(scheduled);
            resize.disconnect();
            for (const event of events) document.removeEventListener(event, schedule, true);
            window.removeEventListener('resize', schedule);
            window.visualViewport?.removeEventListener('resize', schedule);
            window.visualViewport?.removeEventListener('scroll', schedule);
            delete globalThis[key];
        },
    };

    schedule();
})()
