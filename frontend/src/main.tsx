import { useEffect, useRef, useState } from 'react';
import { createRoot } from 'react-dom/client';

import { startRemote, type ConnectionState } from './remote';
import { SessionToolbar } from './SessionToolbar';

import './style.css';

const statusLabels: Record<ConnectionState, string> = {
    connecting: '正在连接',
    streaming: '已连接',
    occupied: '桌面正在被控制',
    disconnected: '连接中断，正在重试',
    unsupported: '浏览器不支持视频解码',
    error: '画面加载失败',
};

function App() {
    const stageRef = useRef<HTMLDivElement>(null);
    const screenRef = useRef<HTMLVideoElement>(null);
    const keyboardRef = useRef<HTMLTextAreaElement>(null);
    const remoteRef = useRef<ReturnType<typeof startRemote> | null>(null);

    const [status, setStatus] = useState<ConnectionState>('connecting');
    const [detail, setDetail] = useState('');
    const [size, setSize] = useState<[number, number] | null>(null);

    useEffect(() => {
        const remote = startRemote(
            stageRef.current!,
            screenRef.current!,
            keyboardRef.current!,
            (next, message) => { setStatus(next); setDetail(message ?? ''); },
            (width, height) => setSize([width, height]),
        );

        remoteRef.current = remote;

        return () => { remote.dispose(); remoteRef.current = null; };
    }, []);

    const waiting = status === 'connecting' || status === 'disconnected';

    return (
        <main className="remote-desktop" aria-label="Codex 远程桌面">
            <div className="stage" ref={stageRef} inert={status !== 'streaming'}>
                <video id="screen" ref={screenRef} aria-label="Codex 桌面画面" autoPlay muted playsInline />
                <textarea id="keyboard" ref={keyboardRef} aria-label="远程键盘输入" autoCapitalize="off" autoComplete="off" spellCheck={false} />
            </div>

            <SessionToolbar status={status} label={statusLabels[status]} size={size} />

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

                        <h1 id="session-title">{statusLabels[status]}</h1>

                        <p id="session-description">{detail || (status === 'occupied'
                            ? '另一个页面拥有控制权。接管后，你可以在这里继续操作，原页面将停止控制。'
                            : status === 'disconnected' ? '正在恢复桌面连接，请稍候。' : '正在准备你的远程桌面…')}</p>

                        {status === 'occupied' && (
                            <button className="primary-button" onClick={() => remoteRef.current?.claim()}>抢占控制权</button>
                        )}

                        {status === 'error' && (
                            <button className="primary-button" onClick={() => location.reload()}>重新连接</button>
                        )}
                    </section>
                </div>
            )}
        </main>
    );
}

createRoot(document.getElementById('root')!).render(<App />);
