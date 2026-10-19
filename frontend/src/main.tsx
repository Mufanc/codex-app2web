import { useEffect, useRef, useState } from 'react';
import { createRoot } from 'react-dom/client';
import { useTranslation } from 'react-i18next';

import './i18n.ts';

import { startRemote, type ConnectionState, type VideoTransport } from './remote';
import { SessionToolbar } from './SessionToolbar';

import './style.css';

function App() {
    const { t } = useTranslation();

    const stageRef = useRef<HTMLDivElement>(null);
    const screenRef = useRef<HTMLVideoElement>(null);
    const keyboardRef = useRef<HTMLTextAreaElement>(null);
    const remoteRef = useRef<ReturnType<typeof startRemote> | null>(null);

    const [status, setStatus] = useState<ConnectionState>('connecting');
    const [transport, setTransport] = useState<VideoTransport | null>(null);
    const [detail, setDetail] = useState('');
    const [size, setSize] = useState<[number, number] | null>(null);

    useEffect(() => {
        const remote = startRemote(
            stageRef.current!,
            screenRef.current!,
            keyboardRef.current!,
            (next, message) => { setStatus(next); setDetail(message ?? ''); },
            (width, height) => setSize([width, height]),
            setTransport,
        );

        remoteRef.current = remote;

        return () => { remote.dispose(); remoteRef.current = null; };
    }, []);

    const waiting = status === 'connecting' || status === 'disconnected';

    return (
        <main className="remote-desktop" aria-label={t('accessibility.desktop')}>
            <div className="stage" ref={stageRef} inert={status !== 'streaming'}>
                <video id="screen" ref={screenRef} aria-label={t('accessibility.screen')} autoPlay muted playsInline />
                <textarea id="keyboard" ref={keyboardRef} aria-label={t('accessibility.keyboard')} autoCapitalize="off" autoComplete="off" spellCheck={false} />
            </div>

            <SessionToolbar status={status} label={status === 'streaming' && transport ? t('status.transport', { transport }) : status === 'connecting' && transport ? detail : t(`status.${status}`)} size={size} />

            {status !== 'streaming' && (
                <div className="session-overlay">
                    <section className="session-card" aria-labelledby="session-title" aria-describedby="session-description">
                        <span className={`session-symbol ${waiting ? 'loading' : ''}`} aria-hidden="true">
                            {!waiting && (
                                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5">
                                    {status === 'occupied'
                                        ? <><rect x="5" y="10" width="14" height="11" rx="3" /><path d="M8 10V7a4 4 0 0 1 8 0v3M12 14v3" /></>
                                        : <><circle cx="12" cy="12" r="9" /><path d="M12 7v6m0 3v1" /></>}
                                </svg>
                            )}
                        </span>

                        <h1 id="session-title">{t(`status.${status}`)}</h1>

                        <p id="session-description">{detail || (status === 'occupied'
                            ? t('session.occupied')
                            : status === 'disconnected' ? t('session.reconnecting') : t('session.preparing'))}</p>

                        {status === 'occupied' && (
                            <button className="primary-button" onClick={() => remoteRef.current?.claim()}>{t('session.takeover')}</button>
                        )}

                        {status === 'error' && (
                            <button className="primary-button" onClick={() => location.reload()}>{t('session.reconnect')}</button>
                        )}
                    </section>
                </div>
            )}
        </main>
    );
}

createRoot(document.getElementById('root')!).render(<App />);
