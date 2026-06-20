/** First n hex chars of an id, for compact display. */
export function shortId(id: string, n = 8): string {
  return id.length > n ? id.slice(0, n) : id;
}

/** Backend timestamps may be seconds or millis; normalize to millis. */
function toMillis(wall: number): number {
  return wall < 1e12 ? wall * 1000 : wall;
}

export function formatTime(wall: number): string {
  const d = new Date(toMillis(wall));
  return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

export function formatDay(wall: number): string {
  const d = new Date(toMillis(wall));
  const today = new Date();
  const yest = new Date();
  yest.setDate(today.getDate() - 1);
  if (d.toDateString() === today.toDateString()) return "Today";
  if (d.toDateString() === yest.toDateString()) return "Yesterday";
  return d.toLocaleDateString([], { month: "short", day: "numeric" });
}

export function humanSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.ceil(bytes / 1024)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}
