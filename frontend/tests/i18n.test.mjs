import assert from 'node:assert/strict';
import { test } from 'node:test';

import i18n, { t } from '../src/i18n.ts';
import en from '../src/locales/en.json' with { type: 'json' };
import zh from '../src/locales/zh.json' with { type: 'json' };

test('browser language preferences select Chinese or English without cached overrides', async context => {
    const original = Object.getOwnPropertyDescriptor(globalThis, 'navigator');

    context.after(() => {
        if (original) Object.defineProperty(globalThis, 'navigator', original);
        else delete globalThis.navigator;
    });

    for (const [languages, expected] of [
        [['zh-CN', 'en-US'], 'zh'],
        [['zh-TW'], 'zh'],
        [['en-GB', 'zh-CN'], 'en'],
        [['fr-FR', 'zh-CN', 'en'], 'zh'],
        [['ja-JP'], 'en'],
        [[], 'en'],
    ]) {
        Object.defineProperty(globalThis, 'navigator', {
            configurable: true,
            value: { languages, language: languages[0] },
        });

        await i18n.changeLanguage();
        assert.equal(i18n.resolvedLanguage, expected, languages.join(','));
        assert.equal(t('session.takeover'), expected === 'zh' ? '抢占控制权' : 'Take control');
    }

    assert.deepEqual(i18n.options.detection.order, ['navigator']);
    assert.deepEqual(i18n.options.detection.caches, []);
});

test('translation catalogs contain matching keys and interpolation variables', () => {
    const flatten = (object, prefix = '') => Object.entries(object).flatMap(([key, value]) =>
        typeof value === 'string' ? [[prefix + key, value]] : flatten(value, `${prefix}${key}.`));
    const english = Object.fromEntries(flatten(en));
    const chinese = Object.fromEntries(flatten(zh));

    assert.deepEqual(Object.keys(english).sort(), Object.keys(chinese).sort());

    for (const [key, value] of Object.entries(english)) {
        assert.ok(value.trim() && chinese[key].trim(), key);
        assert.deepEqual(value.match(/{{\w+}}/g)?.sort(), chinese[key].match(/{{\w+}}/g)?.sort(), key);
    }

    assert.equal(t('toolbar.resolution', { lng: 'en', width: 1920, height: 1080 }), 'Remote 1920 × 1080');
    assert.equal(t('video.decodeError', { lng: 'zh', error: 'test' }), 'WebSocket 视频解码失败：test');
});
