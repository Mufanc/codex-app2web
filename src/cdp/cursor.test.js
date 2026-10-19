(async () => {
    const assert = (value, message) => { if (!value) throw new Error(message); };
    const near = (actual, expected, message) => assert(Math.abs(actual - expected) < 2, `${message}: ${actual} vs ${expected}`);
    const settle = () => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    const point = () => points.at(-1);
    const input = document.querySelector('#input');
    const textarea = document.querySelector('#textarea');
    const editable = document.querySelector('#editable');

    input.focus();
    input.setSelectionRange(0, 0);
    await settle();
    const start = point();
    near(start.x, input.getBoundingClientRect().left + 3 + 19, 'input padding and border');
    input.setSelectionRange(5, 5);
    await settle();
    assert(point().x > start.x + 40, 'input caret follows selection');
    input.setSelectionRange(0, 5, 'backward');
    await settle();
    near(point().x, start.x, 'backward input selection uses focus end');

    input.value = 'a'.repeat(100);
    input.setSelectionRange(40, 40);
    input.dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
    const unscrolled = point();
    const oldScroll = input.scrollLeft;
    input.scrollLeft = oldScroll + 30;
    await settle();
    near(point().x, unscrolled.x - (input.scrollLeft - oldScroll), 'input horizontal scrolling');

    textarea.value = 'abcdefghij '.repeat(20);
    textarea.focus();
    textarea.setSelectionRange(0, 0);
    await settle();
    const firstLine = point();
    textarea.setSelectionRange(35, 35);
    await settle();
    assert(point().y > firstLine.y + 20, 'textarea wraps');
    const beforeScroll = point();
    const oldTop = textarea.scrollTop;
    textarea.scrollTop += 28;
    await settle();
    near(point().y, beforeScroll.y - (textarea.scrollTop - oldTop), 'textarea scroll offset');

    editable.focus();
    const selection = getSelection();
    selection.setBaseAndExtent(editable.firstChild, 8, editable.firstChild, 2);
    await settle();
    const range = document.createRange();
    range.setStart(editable.firstChild, 2);
    range.collapse(true);
    near(point().x, range.getBoundingClientRect().left, 'contenteditable focus end');
    near(point().y, range.getBoundingClientRect().bottom, 'contenteditable baseline');

    editable.replaceChildren(document.createElement('br'));
    selection.setPosition(editable, 0);
    await settle();
    assert(Number.isFinite(point().x) && Number.isFinite(point().y), 'empty editable has a position');
    assert(document.querySelectorAll('html > div').length === 0, 'mirror removed');

    globalThis.__codexApp2WebCursor.stop();
    const count = points.length;
    input.focus();
    input.setSelectionRange(3, 3);
    input.dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
    assert(points.length === count, 'stopped tracker does not publish');
    return true;
})()
