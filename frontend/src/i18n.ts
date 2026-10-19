import i18n from 'i18next';
import LanguageDetector from 'i18next-browser-languagedetector';
import { initReactI18next } from 'react-i18next';

import en from './locales/en.json' with { type: 'json' };
import zh from './locales/zh.json' with { type: 'json' };

declare module 'i18next' {
    interface CustomTypeOptions {
        resources: { translation: typeof en };
        strictKeyChecks: true;
    }
}

void i18n.use(LanguageDetector).use(initReactI18next).init({
    resources: { en: { translation: en }, zh: { translation: zh } },
    supportedLngs: ['en', 'zh'],
    fallbackLng: 'en',
    load: 'languageOnly',
    detection: { order: ['navigator'], caches: [] },
    interpolation: { escapeValue: false },
    initAsync: false,
});

if (typeof document !== 'undefined') {
    document.documentElement.lang = i18n.resolvedLanguage === 'zh' ? 'zh-CN' : 'en';
}

export const t = i18n.t.bind(i18n);

export default i18n;
