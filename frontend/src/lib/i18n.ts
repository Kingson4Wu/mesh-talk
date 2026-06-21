import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import en from "@/locales/en.json";
import zh from "@/locales/zh.json";

export const SUPPORTED_LANGUAGES = ["en", "zh"] as const;
export type Language = (typeof SUPPORTED_LANGUAGES)[number];

const KEY = "mesh-talk-lang";

/** Persisted choice, else the browser's preferred language, else English. */
function initialLanguage(): Language {
  if (typeof localStorage !== "undefined") {
    const stored = localStorage.getItem(KEY);
    if (stored === "en" || stored === "zh") return stored;
  }
  const nav =
    typeof navigator !== "undefined" ? navigator.language.toLowerCase() : "";
  if (nav.startsWith("zh")) return "zh";
  return "en";
}

void i18n.use(initReactI18next).init({
  resources: {
    en: { translation: en },
    zh: { translation: zh },
  },
  lng: initialLanguage(),
  fallbackLng: "en",
  interpolation: { escapeValue: false }, // React already escapes
});

/** Switch and persist the active language. */
export function setLanguage(lng: Language) {
  if (typeof localStorage !== "undefined") localStorage.setItem(KEY, lng);
  void i18n.changeLanguage(lng);
}

export default i18n;
