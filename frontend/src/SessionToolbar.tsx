import { useEffect, useRef, useState } from 'react';

import type { ConnectionState } from './remote';

type Props = {
    status: ConnectionState;
    label: string;
    size: [number, number] | null;
};

export function SessionToolbar({ status, label, size }: Props) {
    const toolbarRef = useRef<HTMLDivElement>(null);
    const panelRef = useRef<HTMLDivElement>(null);
    const triggerRef = useRef<HTMLButtonElement>(null);
    const hideTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);

    const [open, setOpen] = useState(false);
    const [fullscreen, setFullscreen] = useState(false);
    const [fullscreenError, setFullscreenError] = useState('');

    useEffect(() => {
        if (open && document.activeElement === triggerRef.current) {
            panelRef.current?.querySelector('button')?.focus();
        }
    }, [open]);

    useEffect(() => {
        const onFullscreenChange = () => setFullscreen(Boolean(document.fullscreenElement));

        document.addEventListener('fullscreenchange', onFullscreenChange);

        return () => {
            document.removeEventListener('fullscreenchange', onFullscreenChange);
            clearTimeout(hideTimer.current);
        };
    }, []);

    function close() {
        clearTimeout(hideTimer.current);

        if (toolbarRef.current?.contains(document.activeElement)) triggerRef.current?.focus();

        setOpen(false);
    }

    async function toggleFullscreen() {
        try {
            setFullscreenError('');

            if (document.fullscreenElement) await document.exitFullscreen();
            else await document.documentElement.requestFullscreen();
        } catch {
            setFullscreenError('无法切换全屏，请使用浏览器的全屏功能。');
        }
    }

    return (
        <div
            ref={toolbarRef}
            className={`session-toolbar ${open ? 'is-open' : ''}`}
            onPointerEnter={event => {
                clearTimeout(hideTimer.current);

                if (event.pointerType === 'mouse') setOpen(true);
            }}
            onPointerLeave={() => {
                hideTimer.current = setTimeout(() => {
                    if (!toolbarRef.current?.contains(document.activeElement)) setOpen(false);
                }, 300);
            }}
            onBlur={event => {
                if (!event.currentTarget.contains(event.relatedTarget)) setOpen(false);
            }}
            onKeyDown={event => {
                if (event.key === 'Escape') {
                    event.preventDefault();
                    event.stopPropagation();
                    close();
                }
            }}
        >
            <div className="toolbar-edge" />

            <button
                className="toolbar-trigger"
                ref={triggerRef}
                aria-label={open ? '收起会话工具栏' : '展开会话工具栏'}
                aria-expanded={open}
                aria-controls="session-controls"
                onClick={() => setOpen(value => !value)}
            >
                <span className={`connection-dot ${status === 'streaming' ? 'online' : ''}`} />
                <span className="trigger-grip" />
            </button>

            <div id="session-controls" className="toolbar-panel" ref={panelRef} inert={!open}>
                <div className="toolbar-row">
                    <div className="brand">
                        <span className="brand-mark" aria-hidden="true"><span /></span>
                        <div><strong>CodexApp2Web</strong><small>远程桌面</small></div>
                    </div>

                    <div className="session-info">
                        <span className="connection-state">
                            <span className={`connection-dot ${status === 'streaming' ? 'online' : ''}`} />{label}
                        </span>

                        {size && <span className="resolution">远端 {size[0]} × {size[1]}</span>}
                    </div>

                    <div className="toolbar-actions">
                        <button className="icon-button" onClick={() => void toggleFullscreen()} aria-label={fullscreen ? '退出全屏' : '进入全屏'} title={fullscreen ? '退出全屏' : '进入全屏'}>
                            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" aria-hidden="true">
                                <path d={fullscreen ? 'M4 9h5V4m11 5h-5V4M4 15h5v5m11-5h-5v5' : 'M9 4H4v5m11-5h5v5M4 15v5h5m11-5v5h-5'} />
                            </svg>
                        </button>

                        <button className="icon-button" onClick={close} aria-label="收起工具栏" title="收起工具栏">
                            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" aria-hidden="true"><path d="m6 14 6-6 6 6" /></svg>
                        </button>
                    </div>
                </div>

                {fullscreenError && <p className="toolbar-hint" role="alert">{fullscreenError}</p>}
            </div>

            <span className="sr-only" role="status">{label}</span>
        </div>
    );
}
