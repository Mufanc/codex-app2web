import type { Caret, InputMessage, Point } from './protocol.ts';

type Dimensions = { sourceWidth: number; sourceHeight: number; decodedSize: [number, number] | null };
type Picture = { left: number; top: number; width: number; height: number };

export function createInput(
    stage: HTMLElement,
    keyboard: HTMLTextAreaElement,
    dimensions: () => Dimensions,
    send: (message: InputMessage) => void,
) {
    const pressed = new Set<string>();
    let composing = false;
    let compositionTimer: ReturnType<typeof setTimeout> | undefined;
    let caret: Caret | null = null;

    function pictureRect(): Picture | null {
        const { sourceWidth, sourceHeight, decodedSize: size } = dimensions();

        if (!size || !sourceWidth || !sourceHeight) return null;

        const area = stage.getBoundingClientRect();
        const videoScale = Math.min(area.width / size[0], area.height / size[1]);
        const width = size[0] * videoScale;
        const height = size[1] * videoScale;

        return { left: (area.width - width) / 2, top: (area.height - height) / 2, width, height };
    }

    function coordinates(event: PointerEvent | WheelEvent, clamp = false): Point | null {
        const { sourceWidth, sourceHeight } = dimensions();
        const rect = pictureRect();

        if (!rect) return null;

        const area = stage.getBoundingClientRect();
        const x = event.clientX - area.left - rect.left;
        const y = event.clientY - area.top - rect.top;

        if (!clamp && (x < 0 || y < 0 || x > rect.width || y > rect.height)) return null;

        return {
            x: (Math.max(0, Math.min(rect.width, x)) / rect.width) * sourceWidth,
            y: (Math.max(0, Math.min(rect.height, y)) / rect.height) * sourceHeight,
        };
    }

    function placeKeyboard(point: Caret | null) {
        caret = point;

        const { sourceWidth, sourceHeight } = dimensions();
        const rect = pictureRect();

        if (!rect || !point) return;

        const scale = rect.height / sourceHeight;
        const height =
            typeof point.height === 'number' && Number.isFinite(point.height) && point.height > 0
                ? point.height * scale
                : 20;
        const fontSize =
            typeof point.fontSize === 'number' && Number.isFinite(point.fontSize) && point.fontSize > 0
                ? point.fontSize * scale
                : height * 0.8;

        keyboard.style.height = `${height}px`;
        keyboard.style.lineHeight = `${height}px`;
        keyboard.style.fontSize = `${fontSize}px`;
        keyboard.style.left = `${Math.max(0, Math.min(stage.clientWidth - 2, rect.left + (point.x / sourceWidth) * rect.width))}px`;
        // Remote caret coordinates mark its bottom; align the local input's bottom with it.
        keyboard.style.top = `${Math.max(0, Math.min(stage.clientHeight - height, rect.top + (point.y / sourceHeight) * rect.height - height))}px`;
        keyboard.style.maxWidth = `${Math.max(2, stage.clientWidth - parseFloat(keyboard.style.left))}px`;
    }

    function modifiers(event: KeyboardEvent) {
        return (event.altKey ? 1 : 0) | (event.ctrlKey ? 2 : 0) | (event.metaKey ? 4 : 0) | (event.shiftKey ? 8 : 0);
    }

    function onKeyDown(event: KeyboardEvent) {
        if (event.isComposing || composing || event.key === 'Process' || event.key === 'Dead') return;
        if (!(event.metaKey || event.ctrlKey || event.altKey || event.key.length !== 1)) return;

        event.preventDefault();
        pressed.add(event.code);

        send({
            type: 'key',
            action: 'keyDown',
            key: event.key,
            code: event.code,
            key_code: event.keyCode,
            modifiers: modifiers(event),
        });
    }

    function onKeyUp(event: KeyboardEvent) {
        if (!pressed.delete(event.code)) return;

        event.preventDefault();
        send({
            type: 'key',
            action: 'keyUp',
            key: event.key,
            code: event.code,
            key_code: event.keyCode,
            modifiers: modifiers(event),
        });
    }

    function sendKeyboardText() {
        if (!keyboard.value) return;

        send({ type: 'text', text: keyboard.value });
        keyboard.value = '';
    }

    function onCompositionStart() {
        composing = true;
        keyboard.dataset.composing = 'true';
    }

    function onCompositionEnd() {
        composing = false;
        delete keyboard.dataset.composing;
        keyboard.style.width = '';
        compositionTimer = setTimeout(sendKeyboardText, 0);
    }

    function onInput(event: Event) {
        if (composing) {
            keyboard.style.width = '2px';
            keyboard.style.width = `${keyboard.scrollWidth + 2}px`;
        }

        if (!(event as InputEvent).isComposing && !composing) sendKeyboardText();
    }

    function onPointerDown(event: PointerEvent) {
        const point = coordinates(event);

        if (!point) return;

        event.preventDefault();
        placeKeyboard(point);
        keyboard.focus({ preventScroll: true });
        stage.setPointerCapture(event.pointerId);
        send({
            type: 'mouse',
            action: 'mousePressed',
            ...point,
            button: event.button === 2 ? 'right' : event.button === 1 ? 'middle' : 'left',
            buttons: event.buttons,
        });
    }

    function onPointerMove(event: PointerEvent) {
        const point = coordinates(event, stage.hasPointerCapture(event.pointerId));

        if (point) send({ type: 'mouse', action: 'mouseMoved', ...point, button: 'none', buttons: event.buttons });
    }

    function onPointerUp(event: PointerEvent) {
        const point = coordinates(event, true);

        if (point)
            send({
                type: 'mouse',
                action: 'mouseReleased',
                ...point,
                button: event.button === 2 ? 'right' : event.button === 1 ? 'middle' : 'left',
                buttons: event.buttons,
            });
    }

    function onWheel(event: WheelEvent) {
        const point = coordinates(event);

        if (!point) return;

        event.preventDefault();
        send({ type: 'wheel', ...point, delta_x: event.deltaX, delta_y: event.deltaY });
    }

    function onContextMenu(event: MouseEvent) {
        event.preventDefault();
    }

    keyboard.addEventListener('keydown', onKeyDown);
    keyboard.addEventListener('keyup', onKeyUp);
    keyboard.addEventListener('compositionstart', onCompositionStart);
    keyboard.addEventListener('compositionend', onCompositionEnd);
    keyboard.addEventListener('input', onInput);

    stage.addEventListener('pointerdown', onPointerDown);
    stage.addEventListener('pointermove', onPointerMove);
    stage.addEventListener('pointerup', onPointerUp);
    stage.addEventListener('wheel', onWheel, { passive: false });
    stage.addEventListener('contextmenu', onContextMenu);

    function reset(notify = true) {
        clearTimeout(compositionTimer);
        pressed.clear();
        composing = false;
        caret = null;
        keyboard.value = '';
        delete keyboard.dataset.composing;
        keyboard.style.width = '';
        if (notify) send({ type: 'reset' });
    }

    const onBlur = () => reset();
    const onVisibility = () => {
        if (document.hidden) reset();
    };

    window.addEventListener('blur', onBlur);
    document.addEventListener('visibilitychange', onVisibility);
    keyboard.addEventListener('blur', onBlur);
    stage.addEventListener('pointercancel', onBlur);

    return {
        pictureRect,
        placeKeyboard,
        refresh() {
            placeKeyboard(caret);
        },
        reset,
        dispose() {
            reset();
            window.removeEventListener('blur', onBlur);
            document.removeEventListener('visibilitychange', onVisibility);
            keyboard.removeEventListener('blur', onBlur);
            stage.removeEventListener('pointercancel', onBlur);
            keyboard.removeEventListener('keydown', onKeyDown);
            keyboard.removeEventListener('keyup', onKeyUp);
            keyboard.removeEventListener('compositionstart', onCompositionStart);
            keyboard.removeEventListener('compositionend', onCompositionEnd);
            keyboard.removeEventListener('input', onInput);
            stage.removeEventListener('pointerdown', onPointerDown);
            stage.removeEventListener('pointermove', onPointerMove);
            stage.removeEventListener('pointerup', onPointerUp);
            stage.removeEventListener('wheel', onWheel);
            stage.removeEventListener('contextmenu', onContextMenu);
        },
    };
}
