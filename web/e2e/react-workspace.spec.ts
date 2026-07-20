import { expect, test } from "@playwright/test";

test.beforeEach(async ({ page }) => {
  await page.goto("/?demo=complete");
  await expect(page.locator(".workspace-shell")).toBeVisible();
  await expect(page.locator("[data-conversation-turn]")).toHaveCount(5);
});

test("renders the migrated workspace without overflow or runtime errors", async ({ page }, testInfo) => {
  const pageErrors: string[] = [];
  page.on("pageerror", (error) => pageErrors.push(error.message));
  const geometry = await page.evaluate(() => ({
    bodyWidth: document.body.scrollWidth,
    viewportWidth: window.innerWidth,
    bodyHeight: document.body.scrollHeight,
    viewportHeight: window.innerHeight,
  }));
  expect(geometry.bodyWidth).toBeLessThanOrEqual(geometry.viewportWidth);
  expect(geometry.bodyHeight).toBeLessThanOrEqual(geometry.viewportHeight);
  expect(pageErrors).toEqual([]);

  if (testInfo.project.name === "mobile") {
    await expect(page.locator(".turn-navigator")).toBeHidden();
    await page.getByTitle("对话列表").click();
    await expect(page.locator(".conversation-sidebar")).toHaveClass(/is-open/);
    await expect(page.locator(".mobile-scrim")).toBeVisible();
    await page.locator(".mobile-scrim").click();
    await page.getByTitle("研究概览").click();
    await expect(page.locator(".research-inspector")).toBeVisible();
    await expect(page.locator(".conversation-sidebar")).not.toHaveClass(/is-open/);
    await page.locator(".research-inspector-header").getByTitle("关闭").click();
    await page.getByTitle("对话列表").click();
    await expect(page.locator(".research-inspector")).toHaveCount(0);
    await expect(page.locator(".conversation-sidebar")).toHaveClass(/is-open/);
  } else {
    await expect(page.locator(".turn-navigator-heading strong")).toHaveText("05");
  }
});

test("expands the position indicator and jumps between turns", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name === "mobile", "Turn navigator is intentionally hidden on mobile");
  const rail = page.locator(".workspace > .scroll-position-rail");
  const thumb = rail.locator(".scroll-position-thumb");
  await expect(rail).toBeVisible();
  const collapsed = await rail.boundingBox();
  expect(collapsed?.width).toBeLessThanOrEqual(13);
  await rail.hover({ position: { x: 2, y: 20 } });
  await expect.poll(async () => (await rail.boundingBox())?.width).toBeGreaterThanOrEqual(27);
  await expect.poll(async () => (await thumb.boundingBox())?.width).toBeGreaterThanOrEqual(13);

  const transcript = page.locator("#conversation-transcript");
  const initialScrollTop = await transcript.evaluate((element) => element.scrollTop);
  const thumbBox = await thumb.boundingBox();
  expect(initialScrollTop).toBeGreaterThan(0);
  expect(thumbBox).not.toBeNull();
  await page.mouse.move(thumbBox!.x + thumbBox!.width / 2, thumbBox!.y + thumbBox!.height / 2);
  await page.mouse.down();
  await page.mouse.move(thumbBox!.x + thumbBox!.width / 2, thumbBox!.y + thumbBox!.height / 2 - 80);
  await page.mouse.up();
  await expect.poll(async () => transcript.evaluate((element) => element.scrollTop)).toBeLessThan(initialScrollTop);

  const navigator = page.locator(".turn-navigator-panel");
  await navigator.hover({ position: { x: 195, y: 20 } });
  await page.locator(".turn-navigator-list button").first().click();
  await expect(page.locator(".turn-navigator-heading strong")).toHaveText("01", { timeout: 3000 });
});

test("loads overview and audit only after opening the inspector", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name === "mobile", "Desktop and compact layouts cover the inspector geometry");
  await expect(page.getByText("问题理解")).toHaveCount(0);
  await page.getByTitle("研究概览").click();
  await expect(page.getByText("问题理解")).toBeVisible();
  await page.getByRole("tab", { name: "审计详情" }).click();
  await expect(page.getByText("问题理解完成")).toBeVisible();
  const geometry = await page.evaluate(() => ({ body: document.body.scrollWidth, viewport: window.innerWidth }));
  expect(geometry.body).toBeLessThanOrEqual(geometry.viewport);
});

test("wraps long Chinese content while preserving question and answer alignment", async ({ page }, testInfo) => {
  await page.goto("/?demo=long");
  await expect(page.locator("[data-conversation-turn]")).toHaveCount(5);
  const geometry = await page.evaluate(() => {
    const lastTurn = document.querySelector<HTMLElement>("[data-conversation-turn]:last-child")!;
    const question = lastTurn.querySelector<HTMLElement>(".question-message")!.getBoundingClientRect();
    const questionHeading = lastTurn.querySelector<HTMLElement>(".question-message h2")!;
    const answer = lastTurn.querySelector<HTMLElement>(".research-answer")!.getBoundingClientRect();
    const title = document.querySelector<HTMLElement>(".document-title h1")!;
    const titleStyle = getComputedStyle(title);
    const wrappingTargets = [...document.querySelectorAll<HTMLElement>(".question-message h2, .answer-prose")];
    return {
      bodyWidth: document.body.scrollWidth,
      viewportWidth: window.innerWidth,
      questionLeft: question.left,
      questionTextAlign: getComputedStyle(questionHeading).textAlign,
      answerLeft: answer.left,
      titleFitsOrEllipsizes: title.scrollWidth <= title.clientWidth + 1
        || (titleStyle.overflow === "hidden" && titleStyle.textOverflow === "ellipsis"),
      overflowing: wrappingTargets
        .filter((element) => element.scrollWidth > element.clientWidth + 1)
        .map((element) => ({
          className: element.className,
          tagName: element.tagName,
          clientWidth: element.clientWidth,
          scrollWidth: element.scrollWidth,
          text: element.textContent?.slice(0, 80),
        })),
    };
  });
  expect(geometry.bodyWidth).toBeLessThanOrEqual(geometry.viewportWidth);
  expect(geometry.questionTextAlign).toBe("right");
  if (testInfo.project.name !== "mobile") {
    expect(geometry.questionLeft).toBeGreaterThan(geometry.answerLeft);
  }
  expect(geometry.titleFitsOrEllipsizes).toBe(true);
  expect(geometry.overflowing).toEqual([]);
});
