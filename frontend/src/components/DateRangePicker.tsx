import { useState } from "react";
import { format } from "date-fns";
import { CalendarDays } from "lucide-react";
import type { DateRange } from "react-day-picker";
import { useIsMobile } from "../hooks/use-mobile";
import { Button } from "./ui/button";
import { Calendar } from "./ui/calendar";
import { Popover, PopoverContent, PopoverTrigger } from "./ui/popover";

const presets = [
  { value: "24h", label: "Last 24 hours" },
  { value: "7d", label: "Last 7 days" },
  { value: "1m", label: "Last 1 month" },
  { value: "6m", label: "Last 6 months" },
  { value: "1y", label: "Last 1 year" },
] as const;
type Preset = (typeof presets)[number]["value"];
export type DashboardRange =
  | { kind: "preset"; value: Preset }
  | { kind: "custom"; from: number; until: number };

export function rangeStart(range: Preset): number {
  const date = new Date();
  if (range === "24h" || range === "7d") {
    return Math.floor(
      (date.getTime() - (range === "24h" ? 24 : 7 * 24) * 3600_000) / 1000,
    );
  }
  const day = date.getUTCDate();
  date.setUTCDate(1);
  date.setUTCMonth(
    date.getUTCMonth() - (range === "1m" ? 1 : range === "6m" ? 6 : 12),
  );
  const lastDay = new Date(
    Date.UTC(date.getUTCFullYear(), date.getUTCMonth() + 1, 0),
  ).getUTCDate();
  date.setUTCDate(Math.min(day, lastDay));
  return Math.floor(date.getTime() / 1000);
}

function datesFor(range: DashboardRange): DateRange {
  return range.kind === "preset"
    ? { from: new Date(rangeStart(range.value) * 1000), to: new Date() }
    : { from: new Date(range.from * 1000), to: new Date(range.until * 1000) };
}

export function DateRangePicker({
  value,
  onChange,
}: {
  value: DashboardRange;
  onChange: (range: DashboardRange) => void;
}) {
  const [open, setOpen] = useState(false);
  const [draft, setDraft] = useState<DateRange | undefined>();
  const mobile = useIsMobile();
  const selected = draft ?? datesFor(value);
  const label =
    value.kind === "preset"
      ? presets.find((preset) => preset.value === value.value)?.label
      : `${format(new Date(value.from * 1000), "LLL d, y")} – ${format(new Date(value.until * 1000), "LLL d, y")}`;

  return (
    <Popover
      open={open}
      onOpenChange={(next) => {
        setOpen(next);
        if (next) setDraft(undefined);
      }}
    >
      <PopoverTrigger
        render={
          <Button
            variant="outline"
            aria-label={`Dashboard time range: ${label}`}
          />
        }
      >
        <CalendarDays /> {label}
      </PopoverTrigger>
      <PopoverContent
        align="end"
        className="w-auto max-w-[calc(100vw-1rem)] p-0"
      >
        <div className="flex max-w-full">
          <Calendar
            className="[--cell-size:--spacing(6)] sm:[--cell-size:--spacing(7)]"
            mode="range"
            selected={selected}
            onSelect={(next, day) =>
              setDraft(draft ? next : { from: day, to: day })
            }
            defaultMonth={value.kind === "custom" ? selected.from : selected.to}
            numberOfMonths={mobile ? 1 : 2}
          />
          <div className="flex w-28 shrink-0 flex-col gap-1 border-l p-1 sm:w-36 sm:p-2">
            {presets.map((preset) => (
              <Button
                key={preset.value}
                variant={
                  value.kind === "preset" && value.value === preset.value
                    ? "secondary"
                    : "ghost"
                }
                size="sm"
                className="justify-start px-1 text-xs sm:px-2.5 sm:text-sm"
                onClick={() => {
                  onChange({ kind: "preset", value: preset.value });
                  setOpen(false);
                }}
              >
                {preset.label}
              </Button>
            ))}
            <Button
              size="sm"
              className="mt-auto"
              disabled={!draft?.from || !draft.to}
              onClick={() => {
                if (!draft?.from || !draft.to) return;
                const from = draft.from;
                const to = draft.to;
                onChange({
                  kind: "custom",
                  from: Math.floor(
                    new Date(
                      from.getFullYear(),
                      from.getMonth(),
                      from.getDate(),
                    ).getTime() / 1000,
                  ),
                  until:
                    Math.floor(
                      new Date(
                        to.getFullYear(),
                        to.getMonth(),
                        to.getDate() + 1,
                      ).getTime() / 1000,
                    ) - 1,
                });
                setOpen(false);
              }}
            >
              Apply dates
            </Button>
          </div>
        </div>
      </PopoverContent>
    </Popover>
  );
}
