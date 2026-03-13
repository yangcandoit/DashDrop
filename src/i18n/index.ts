import { createI18n } from 'vue-i18n';
import zh from './zh.json';

const messages = {
  zh,
  // Later we can add 'en'
};

const i18n = createI18n({
  locale: 'zh', // set default locale to Chinese
  fallbackLocale: 'zh',
  messages,
});

export default i18n;
