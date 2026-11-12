const dateFormat = new Intl.DateTimeFormat("en-US", {
  month: "short",
  day: "numeric",
  year: "numeric",
});

const relativeTimeFormat = new Intl.RelativeTimeFormat("en", {
  numeric: "auto",
});

export function formatDate(date: Date): string {
  return dateFormat.format(date);
}

export function formatDateYMD(date: Date): string {
  const y = date.getFullYear();
  const m = String(date.getMonth() + 1).padStart(2, "0");
  const d = String(date.getDate()).padStart(2, "0");
  return `${y}-${m}-${d}`;
}

export function formatDistanceToNow(
  date: Date,
  options?: { addSuffix?: boolean },
): string {
  const diffSec = Math.round((date.getTime() - Date.now()) / 1000);
  const absSec = Math.abs(diffSec);
  if (absSec < 60) return "just now";
  const absMin = Math.round(absSec / 60);
  if (absMin < 60) {
    return options?.addSuffix
      ? relativeTimeFormat.format(Math.round(diffSec / 60), "minute")
      : `${absMin} minutes`;
  }
  const absHours = Math.round(absMin / 60);
  if (absHours < 24) {
    return options?.addSuffix
      ? relativeTimeFormat.format(Math.round(diffSec / 3600), "hour")
      : `${absHours} hours`;
  }
  const absDays = Math.round(absHours / 24);
  if (absDays < 30) {
    return options?.addSuffix
      ? relativeTimeFormat.format(Math.round(diffSec / 86400), "day")
      : `${absDays} days`;
  }
  const absMonths = Math.round(absDays / 30);
  if (absMonths < 12) {
    return options?.addSuffix
      ? relativeTimeFormat.format(Math.round(diffSec / 2592000), "month")
      : `${absMonths} months`;
  }
  const absYears = Math.round(absMonths / 12);
  return options?.addSuffix
    ? relativeTimeFormat.format(Math.round(diffSec / 31536000), "year")
    : `${absYears} years`;
}

export function formatTimestamp(value: number): string {
  const date = new Date(value * 1000);
  return Number.isNaN(date.getTime()) ? String(value) : date.toLocaleString();
}

export function formatCountdown(minutes: number): string {
  const days = Math.floor(minutes / 1_440);
  const hours = Math.floor((minutes % 1_440) / 60);
  const remainingMinutes = minutes % 60;
  return [
    days > 0 && `${days}d`,
    hours > 0 && `${hours}h`,
    remainingMinutes > 0 && `${remainingMinutes}m`,
  ]
    .filter(Boolean)
    .join(" ");
}
