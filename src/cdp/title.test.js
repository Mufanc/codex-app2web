(async () => {
    const assert = (condition, message) => { if (!condition) throw new Error(message); };
    const settle = () => new Promise(resolve => setTimeout(resolve, 0));

    assert(titles.length === 1 && titles[0] === document.title, 'Initial title');
    document.title = 'Task A';
    await settle();
    assert(titles.at(-1) === 'Task A', 'Title update');

    const replacement = document.createElement('title');
    replacement.textContent = 'Task B';
    document.querySelector('title').replaceWith(replacement);
    await settle();
    assert(titles.at(-1) === 'Task B', 'Title element replacement');

    replacement.remove();
    await settle();
    assert(titles.at(-1) === '', 'Empty title');

    const count = titles.length;
    document.head.append(document.createElement('meta'));
    await settle();
    assert(titles.length === count, 'Unchanged title deduplication');

    globalThis.__codexApp2WebTitle.disconnect();
    document.title = 'Stopped';
    await settle();
    assert(titles.length === count, 'Observer cleanup');
})();
