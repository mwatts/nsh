import { expect, open, test } from "nsh/test";

test("a drag goes to the responder from grant to release", async () => {
  const app = await open("./App.tsx");
  await app.drag("id:handle", { x: 30, y: -5 });
  const log = app.texts().find((t) => t.startsWith("Log: "))!;
  const lines = log.slice(5).split(" | ");
  expect(lines[0]).toBe("grant 50,20");
  expect(lines[lines.length - 2]).toBe("move 80,15");
  expect(lines[lines.length - 1]).toBe("release 80,15");
  expect(lines.length).toBe(5);
});

test("a native pan moves its value with the pointer", async () => {
  const app = await open("./App.tsx");
  await app.drag("id:card", { x: 40, y: 0 });
  expect(app.texts()).toContain("Pan: start 0 at 5; end 40,0 at 45");
});

test("a pan activation makes its view the responder only across", async () => {
  const app = await open("./App.tsx");
  await app.drag("id:scrubber", { x: 0, y: 30 });
  expect(app.texts()).toContain("Scrub: none");
  await app.drag("id:scrubber", { x: -50, y: 0 });
  expect(app.texts()).toContain("Scrub: grant at 80; release at 50");
});

test("a row dragged by its handle onto another moves there", async () => {
  const app = await open("./App.tsx");
  await app.drag("id:handle-A", "id:row-C");
  expect(app.texts()).toContain("Order: BCAD");
  await app.drag("id:handle-D", "id:row-B");
  expect(app.texts()).toContain("Order: DBCA");
});

test("a drag names one view to start from and to end on", async () => {
  const app = await open("./App.tsx");
  await expect(() => app.drag("id:none", { x: 1, y: 1 })).rejects.toThrow("no view matches `id:none`");
  await expect(() => app.drag("id:handle", "id:none")).rejects.toThrow("no view matches `id:none`");
});
