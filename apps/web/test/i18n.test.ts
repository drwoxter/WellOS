import { describe, expect, it } from "vitest";
import { messages } from "@/lib/i18n";

describe("i18n dictionary", () => {
  it("has the same keys in English and Spanish", () => {
    const en = Object.keys(messages.en).sort();
    const es = Object.keys(messages.es).sort();
    expect(es).toEqual(en);
  });

  it("has no empty strings in either language", () => {
    for (const lang of ["en", "es"] as const) {
      for (const [key, value] of Object.entries(messages[lang])) {
        expect(value.trim(), `${lang}.${key}`).not.toBe("");
      }
    }
  });
});
