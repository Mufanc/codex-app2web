import { useEffect, useLayoutEffect, useRef, useState } from 'react';

import type { MenuItem, NativeMenu, Point } from './protocol';

function MenuList({
    items,
    position,
    onSelect,
    onBack,
    focusFirst = true,
}: {
    items: MenuItem[];
    position: Point & { parentLeft?: number };
    onSelect: (id: string | null) => void;
    onBack?: () => void;
    focusFirst?: boolean;
}) {
    const ref = useRef<HTMLDivElement>(null);
    const [submenu, setSubmenu] = useState<({ index: number; parentLeft: number; keyboard?: boolean } & Point) | null>(
        null,
    );

    useLayoutEffect(() => {
        const element = ref.current!;
        const rect = element.getBoundingClientRect();
        const x =
            position.parentLeft !== undefined && position.x + rect.width > innerWidth - 8
                ? position.parentLeft - rect.width - 5
                : position.x;

        element.style.left = `${Math.max(8, Math.min(x, innerWidth - rect.width - 8))}px`;
        element.style.top = `${Math.max(8, Math.min(position.y, innerHeight - rect.height - 8))}px`;
    }, []);

    useLayoutEffect(() => {
        if (focusFirst)
            ref.current?.querySelector<HTMLButtonElement>('button:not(:disabled)')?.focus({ preventScroll: true });
    }, [focusFirst]);

    return (
        <div
            className="native-menu"
            ref={ref}
            role="menu"
            style={{ left: position.x, top: position.y }}
            onKeyDown={(event) => {
                if (event.key === 'Tab' || event.key === 'Escape') {
                    event.preventDefault();
                    event.stopPropagation();
                    onSelect(null);
                }

                if (event.key === 'ArrowLeft' && onBack) {
                    event.preventDefault();
                    event.stopPropagation();
                    onBack();
                }

                if (
                    event.key === 'ArrowDown' ||
                    event.key === 'ArrowUp' ||
                    event.key === 'Home' ||
                    event.key === 'End'
                ) {
                    event.preventDefault();
                    event.stopPropagation();

                    const buttons = [
                        ...ref.current!.querySelectorAll<HTMLButtonElement>(
                            ':scope > .native-menu-row > button:not(:disabled)',
                        ),
                    ];
                    const index = buttons.indexOf(document.activeElement as HTMLButtonElement);
                    const next =
                        event.key === 'Home'
                            ? 0
                            : event.key === 'End'
                              ? buttons.length - 1
                              : (index + (event.key === 'ArrowDown' ? 1 : -1) + buttons.length) % buttons.length;

                    buttons[next]?.focus();
                }
            }}
        >
            {items.map((item, index) =>
                item.type === 'separator' ? (
                    <div role="separator" className="native-menu-separator" key={index} />
                ) : (
                    <div className="native-menu-row" key={index}>
                        <button
                            role={item.type === 'radio' ? 'menuitemradio' : 'menuitem'}
                            aria-checked={item.type === 'radio' ? !!item.checked : undefined}
                            aria-haspopup={item.submenu ? 'menu' : undefined}
                            aria-expanded={item.submenu ? submenu?.index === index : undefined}
                            disabled={item.enabled === false}
                            data-index={index}
                            onPointerEnter={(event) => {
                                if (item.enabled === false) return;
                                event.currentTarget.focus({ preventScroll: true });

                                const rect = event.currentTarget.getBoundingClientRect();

                                setSubmenu(
                                    item.submenu
                                        ? { index, parentLeft: rect.left, x: rect.right + 5, y: rect.top }
                                        : null,
                                );
                            }}
                            onKeyDown={(event) => {
                                if (event.key === 'ArrowRight' && item.submenu) {
                                    event.preventDefault();

                                    const rect = event.currentTarget.getBoundingClientRect();

                                    setSubmenu({
                                        index,
                                        parentLeft: rect.left,
                                        x: rect.right + 5,
                                        y: rect.top,
                                        keyboard: true,
                                    });
                                }
                            }}
                            onClick={(event) => {
                                const rect = event.currentTarget.getBoundingClientRect();

                                if (item.submenu) {
                                    setSubmenu({ index, parentLeft: rect.left, x: rect.right + 5, y: rect.top });
                                } else if (item.id) {
                                    onSelect(item.id);
                                }
                            }}
                        >
                            <span>
                                {item.checked ? '✓ ' : ''}
                                {item.label}
                            </span>
                            <span className="native-menu-shortcut">
                                {item.submenu ? '›' : item.accelerator?.replaceAll('CmdOrCtrl', '⌘')}
                            </span>
                        </button>

                        {item.submenu && submenu?.index === index && (
                            <MenuList
                                position={submenu}
                                focusFirst={submenu.keyboard === true}
                                items={item.submenu}
                                onSelect={onSelect}
                                onBack={() => {
                                    setSubmenu(null);
                                    ref.current
                                        ?.querySelector<HTMLButtonElement>(
                                            `:scope > .native-menu-row > button[data-index="${index}"]`,
                                        )
                                        ?.focus();
                                }}
                            />
                        )}
                    </div>
                ),
            )}
        </div>
    );
}

export function ContextMenu({ menu, onSelect }: { menu: NativeMenu; onSelect: (id: string | null) => void }) {
    useEffect(() => {
        const cancel = () => onSelect(null);

        window.addEventListener('blur', cancel);
        window.addEventListener('resize', cancel);

        return () => {
            window.removeEventListener('blur', cancel);
            window.removeEventListener('resize', cancel);
        };
    }, [onSelect]);

    return (
        <div
            className="native-menu-overlay"
            onPointerDown={() => onSelect(null)}
            onContextMenu={(event) => event.preventDefault()}
        >
            <div onPointerDown={(event) => event.stopPropagation()}>
                <MenuList position={menu} items={menu.items} onSelect={onSelect} />
            </div>
        </div>
    );
}
