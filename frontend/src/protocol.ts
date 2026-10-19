export const MAX_VIDEO_WIDTH = 4096;
export const MAX_VIDEO_HEIGHT = 2160;

export type Point = { x: number; y: number };

export type MenuItem = {
    id?: string;
    label: string;
    type?: 'separator' | 'radio';
    enabled?: boolean;
    checked?: boolean;
    accelerator?: string;
    submenu?: MenuItem[];
};

export type NativeMenu = Point & { id: string; items: MenuItem[] };

export type InputMessage =
    | { type: 'menu_result'; id: string; selected: string | null }
    | { type: 'reset' }
    | { type: 'resize'; width: number; height: number }
    | { type: 'text'; text: string }
    | { type: 'key'; action: 'keyDown' | 'keyUp'; key: string; code: string; key_code: number; modifiers: number }
    | ({
          type: 'mouse';
          action: 'mousePressed' | 'mouseMoved' | 'mouseReleased';
          button: 'left' | 'middle' | 'right' | 'none';
          buttons: number;
      } & Point)
    | ({ type: 'wheel'; delta_x: number; delta_y: number } & Point);

export type ClientMessage =
    | InputMessage
    | { type: 'offer'; video_id: string; width: number; height: number; sdp: string }
    | { type: 'ice'; video_id: string; candidate: RTCIceCandidateInit | null }
    | { type: 'fallback' }
    | { type: 'video_ack'; sequence: number };

export type VideoSignal =
    | { type: 'answer'; video_id: string; description: RTCSessionDescriptionInit }
    | { type: 'ice'; video_id: string; candidate: RTCIceCandidateInit | null };

export type ServerMessage =
    | { type: 'menu'; menu: NativeMenu | null }
    | VideoSignal
    | { type: 'size'; width: number; height: number }
    | { type: 'title'; title: string }
    | { type: 'cursor'; point: Point | null }
    | { type: 'video_error'; code: 'signaling_failed' | 'encoder_failed' | 'transport_failed' };
