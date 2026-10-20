import { fileURLToPath } from 'node:url';

import { defineConfig } from 'vite';
import React from '@vitejs/plugin-react';

export default defineConfig({
    plugins: [React()],
    resolve: {
        alias: {
            '@novnc/encodings': fileURLToPath(new URL('./encodings.js', import.meta.resolve('@novnc/novnc'))),
        },
    },
});
