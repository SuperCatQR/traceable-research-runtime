export class ResearchTraceAuditCache<Page> {
  private readonly pagesByTurn = new Map<string, Map<string, Page>>();

  get(turnId: string, stage: string): Page | undefined {
    return this.pagesByTurn.get(turnId)?.get(stage);
  }

  has(turnId: string, stage: string): boolean {
    return this.pagesByTurn.get(turnId)?.has(stage) ?? false;
  }

  set(turnId: string, stage: string, page: Page): void {
    const pagesByStage = this.pagesByTurn.get(turnId) ?? new Map<string, Page>();
    pagesByStage.set(stage, page);
    this.pagesByTurn.set(turnId, pagesByStage);
  }

  delete(turnId: string, stage: string): void {
    const pagesByStage = this.pagesByTurn.get(turnId);
    if (!pagesByStage) return;
    pagesByStage.delete(stage);
    if (pagesByStage.size === 0) this.pagesByTurn.delete(turnId);
  }

  clear(): void {
    this.pagesByTurn.clear();
  }
}
