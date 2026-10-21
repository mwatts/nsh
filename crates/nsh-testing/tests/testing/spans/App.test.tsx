import { expect, open, test } from "nsh/test";

test("a press on a span goes to the innermost pressable span there", async () => {
  const app = await open("./App.tsx");
  await app.pressSpan("id:docs");
  expect(app.texts()).toContain("Pressed: docs");
  await app.pressSpan("text:here");
  expect(app.texts()).toContain("Pressed: deep");
  await app.pressSpan("id:note >> text:Read");
  expect(app.texts()).toContain("Pressed: note");
  await app.pressSpan("id:docs");
  await app.pressSpan("skip");
  expect(app.texts()).toContain("Pressed: note");
});

test("a span's style is what it inherits and sets", async () => {
  const app = await open("./App.tsx");
  const docs = app.spanStyle("id:docs");
  expect(docs.color).toBe("#2563ebff");
  expect(docs.underline).toBe(true);
  expect(docs.fontWeight).toBe(400);
  const deep = app.spanStyle("id:deep");
  expect(deep.color).toBe("#2563ebff");
  expect(deep.fontWeight).toBe(700);
  expect(app.spanStyle("text:Read").underline).toBe(false);
  expect(app.spanStyle("id:mark").backgroundColor).toBe(undefined);
  await app.press("id:toggle");
  expect(app.spanStyle("id:mark").backgroundColor).toBe("#fde68aff");
  expect(app.spanStyle("text:it.").backgroundColor).toBe(undefined);
});

test("a span selector names one span", async () => {
  const app = await open("./App.tsx");
  await expect(() => app.pressSpan("text:e")).rejects.toThrow("`text:e` matches 6 spans");
  await expect(() => app.pressSpan("id:none")).rejects.toThrow("no span matches `id:none`");
  expect(() => app.spanStyle("label:docs")).toThrow("a span is named by `id:<testID>` or `text:<text>`");
  expect(app.spanStyle("text:/h\\w+/ >> nth=1").fontWeight).toBe(700);
});
