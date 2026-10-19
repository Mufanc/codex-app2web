(() => {
    globalThis.__codexApp2WebTitle?.disconnect();

    let previous;

    const publish = () => {
        const title = document.title;

        if (title === previous) return;

        previous = title;
        globalThis.__codexApp2WebTitleChanged(title);
    };

    const observer = new MutationObserver(publish);

    observer.observe(document.head, { subtree: true, childList: true, characterData: true });
    globalThis.__codexApp2WebTitle = observer;
    publish();
})();
