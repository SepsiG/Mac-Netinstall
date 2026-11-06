// A small typed i18n store: flat message keys, ICU message syntax
// (`{count, plural, one {# device} other {# devices}}`) via
// intl-messageformat. English is the source of truth; a key missing in
// another language falls back to English, then to the key itself.
//
// Why not svelte-i18n: its 4.x release pulls esbuild 0.19 (flagged by
// npm audit) and es5-ext (install script) into the dependency tree for a
// CLI we do not use; this is ~60 lines and checked by tests.
import { IntlMessageFormat } from "intl-messageformat";
import { derived, get, writable } from "svelte/store";
import en from "../locales/en.json";
import hu from "../locales/hu.json";

export type Lang = "en" | "hu";
export const LANGS: readonly Lang[] = ["en", "hu"];
export type MessageKey = keyof typeof en;
export type Values = Record<string, string | number>;

const dictionaries: Record<Lang, Record<string, string>> = { en, hu };
const cache = new Map<string, IntlMessageFormat>();

export const locale = writable<Lang>("en");

/** `en` / `hu` from a setting, else from the macOS preferred languages. */
export function pickLanguage(setting: string | null, preferred: readonly string[]): Lang {
  if (setting === "en" || setting === "hu") return setting;
  const first = preferred[0]?.toLowerCase() ?? "";
  return first.startsWith("hu") ? "hu" : "en";
}

/** Whether `key` has a message (in English, the source of truth). */
export function has(key: string): boolean {
  return key in dictionaries.en;
}

/** The message `key` in `lang`, formatted with `values`. */
export function format(lang: Lang, key: string, values: Values = {}): string {
  const source = dictionaries[lang][key] ?? dictionaries.en[key];
  if (source === undefined) return key;
  const id = `${lang}\u0000${key}`;
  let fmt = cache.get(id);
  if (!fmt) {
    fmt = new IntlMessageFormat(source, lang);
    cache.set(id, fmt);
  }
  try {
    return String(fmt.format(values));
  } catch {
    return source; // a missing value: show the raw text rather than nothing
  }
}

/** `$t(key, values)` in components; follows `locale`. */
export const t = derived(locale, (lang) => (key: MessageKey | string, values?: Values) =>
  format(lang, key, values),
);

/** The current language, outside components. */
export function currentLang(): Lang {
  return get(locale);
}
