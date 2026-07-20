export function formatActivityDate(timestamp: number): string {
  const activityDate = new Date(timestamp * 1000);
  const today = new Date();
  const sharesCalendarDate = activityDate.toDateString() === today.toDateString();
  return new Intl.DateTimeFormat("zh-CN", sharesCalendarDate
    ? { hour: "2-digit", minute: "2-digit" }
    : { month: "short", day: "numeric" }).format(activityDate);
}
export function formatTraceTimestamp(value: string): string {
  const timestamp = new Date(value);
  if (Number.isNaN(timestamp.getTime())) return value;
  return new Intl.DateTimeFormat("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
  }).format(timestamp);
}

export function safeEvidenceUrl(value: string): string | undefined {
  try {
    const parsed = new URL(value);
    return parsed.protocol === "http:" || parsed.protocol === "https:" ? parsed.href : undefined;
  } catch {
    return undefined;
  }
}
