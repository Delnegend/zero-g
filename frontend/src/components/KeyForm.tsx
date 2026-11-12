import { FilterGroup } from "@/components/ObservabilityFilters";
import { Button } from "@/components/ui/button";
import { Calendar } from "@/components/ui/calendar";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { formatDate, formatDateYMD, formatCountdown } from "@/lib/date";
import { useForm } from "@tanstack/react-form";
import { CalendarDays, Clock } from "lucide-react";
import { useEffect, useState } from "react";

export function localDateTime(seconds: number | null | undefined) {
  if (seconds == null) return "";
  const date = new Date(seconds * 1000);
  const local = new Date(date.getTime() - date.getTimezoneOffset() * 60_000);
  return local.toISOString().slice(0, 19);
}

export function expiration(value: string): number | null {
  if (!value) return null;
  if (
    !/^\d{4}-\d{2}-\d{2}T(?:[01]\d|2[0-3]):[0-5]\d(?::[0-5]\d)?$/.test(value)
  ) {
    throw new Error("Enter a valid time as HH:MM or HH:MM:SS.");
  }
  const seconds = Math.floor(new Date(value).getTime() / 1000);
  if (!Number.isFinite(seconds) || seconds <= Date.now() / 1000) {
    throw new Error("Expiration must be in the future.");
  }
  return seconds;
}

export function ExpirationStatus({
  expiresAt,
}: {
  expiresAt: number | null | undefined;
}) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (expiresAt == null) return;
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [expiresAt]);
  if (expiresAt == null) return <span>Never expires</span>;
  const date = new Date(expiresAt * 1000);
  const minutes = Math.ceil((date.getTime() - now) / 60_000);
  const relative = formatCountdown(minutes);
  return (
    <Tooltip>
      <TooltipTrigger className="text-left whitespace-nowrap text-muted-foreground underline decoration-dotted underline-offset-4 focus-visible:outline-2 focus-visible:outline-offset-2">
        {minutes > 0 ? `Expires in ${relative}` : "Expired"}
      </TooltipTrigger>
      <TooltipContent>{date.toLocaleString()}</TooltipContent>
    </Tooltip>
  );
}

function ExpirationField({
  value,
  onChange,
  disabled,
}: {
  value: string;
  onChange: (value: string) => void;
  disabled: boolean;
}) {
  const [open, setOpen] = useState(false);
  const datePart = value.split("T")[0];
  const date = datePart ? new Date(`${datePart}T12:00:00`) : undefined;
  const selected = date && Number.isFinite(date.getTime()) ? date : undefined;
  const time = value.includes("T") ? value.split("T")[1] : "";
  return (
    <div className="grid gap-2">
      <span className="text-sm font-medium">Expiration (optional)</span>
      <Popover open={open} onOpenChange={setOpen}>
        <PopoverTrigger
          render={
            <Button
              type="button"
              variant="outline"
              disabled={disabled}
              aria-label={`Choose expiration date: ${selected ? `${formatDate(selected)} ${time}` : "never expires"}`}
              className="w-full justify-start text-left font-normal"
            />
          }
        >
          <CalendarDays />
          <span className="min-w-0 truncate">
            {selected ? `${formatDate(selected)} · ${time}` : "Never expires"}
          </span>
        </PopoverTrigger>
        <PopoverContent
          align="start"
          className="w-62.5 max-w-[calc(100vw-1rem)] p-0"
        >
          <Calendar
            classNames={{ root: "rdp-root w-full" }}
            mode="single"
            selected={selected}
            defaultMonth={selected}
            onSelect={(day) => {
              if (day) onChange(`${formatDateYMD(day)}T${time || "23:59:59"}`);
            }}
          />
          <Label className="mx-3 flex items-center justify-between gap-3 border-t pt-3 text-sm">
            Time (local)
            <div className="relative flex items-center">
              <Input
                className="h-8 w-32 pr-7 font-mono text-xs"
                type="time"
                step="1"
                aria-label="Time (24-hour)"
                placeholder="HH:MM:SS"
                disabled={disabled || !selected}
                value={time}
                onChange={(event) =>
                  onChange(`${datePart}T${event.target.value}`)
                }
              />
              <Clock
                aria-hidden="true"
                className="pointer-events-none absolute right-2 size-3.5 text-muted-foreground"
              />
            </div>
          </Label>
          <div className="flex justify-between gap-2 px-3 pb-3">
            <Button
              type="button"
              variant="ghost"
              size="sm"
              disabled={disabled || !selected}
              onClick={() => {
                onChange("");
                setOpen(false);
              }}
            >
              Clear expiration
            </Button>
            <Button type="button" size="sm" onClick={() => setOpen(false)}>
              Done
            </Button>
          </div>
        </PopoverContent>
      </Popover>
    </div>
  );
}

export function KeyForm({
  defaultValues,
  submitLabel,
  busy,
  optionsUnavailable,
  providers,
  catalogProviders,
  onCancel,
  onSubmit,
}: {
  defaultValues: {
    name: string;
    providers: string[];
    models: string[];
    expiresAt: string;
  };
  submitLabel: string;
  busy: boolean;
  optionsUnavailable: boolean;
  providers: Array<{ id: string; name: string }>;
  catalogProviders: Array<{
    id: string;
    name: string;
    models: Array<{ model: { id: string } }>;
  }>;
  onCancel: () => void;
  onSubmit: (value: {
    name: string;
    providers: string[];
    models: string[];
    expiresAt: string;
  }) => void;
}) {
  const form = useForm({
    defaultValues,
    onSubmit: ({ value }) => onSubmit(value),
  });

  return (
    <form
      className="grid gap-5"
      onSubmit={(event) => {
        event.preventDefault();
        void form.handleSubmit();
      }}
    >
      <form.Field name="name">
        {(field) => (
          <Label className="block text-sm font-medium">
            Name
            <Input
              className="mt-1"
              disabled={busy}
              required
              value={field.state.value}
              onBlur={field.handleBlur}
              onChange={(event) => field.handleChange(event.target.value)}
            />
          </Label>
        )}
      </form.Field>
      <div>
        <p className="text-xs text-muted-foreground">
          Select none to allow all providers.
        </p>
        <form.Field name="providers">
          {(field) => (
            <FilterGroup
              title="Providers"
              values={providers.map((p) => p.id)}
              identities={providers}
              selected={field.state.value}
              onChange={(nextProviders) => {
                field.handleChange(nextProviders);
                if (nextProviders.length > 0) {
                  const validModels = new Set(
                    catalogProviders
                      .filter((p) => nextProviders.includes(p.id))
                      .flatMap((p) => p.models.map((m) => m.model.id)),
                  );
                  form.setFieldValue("models", (prev) =>
                    prev.filter((id) => validModels.has(id)),
                  );
                }
              }}
              defaultOpen
              disabled={busy}
            />
          )}
        </form.Field>
      </div>
      <div>
        <p className="text-xs text-muted-foreground">
          Select none to allow all models. Model IDs are upstream IDs.
        </p>
        <form.Subscribe selector={(state) => state.values.providers}>
          {(selectedProviders) => {
            const availableModels = [
              ...new Set(
                catalogProviders
                  .filter(
                    (provider) =>
                      selectedProviders.length === 0 ||
                      selectedProviders.includes(provider.id),
                  )
                  .flatMap((provider) =>
                    provider.models.map((model) => model.model.id),
                  ),
              ),
            ];
            return (
              <form.Field name="models">
                {(field) => (
                  <FilterGroup
                    title="Models"
                    values={availableModels}
                    selected={field.state.value}
                    onChange={field.handleChange}
                    defaultOpen
                    disabled={busy}
                  />
                )}
              </form.Field>
            );
          }}
        </form.Subscribe>
      </div>
      {!catalogProviders.some((provider) => provider.models.length > 0) && (
        <p className="text-sm text-muted-foreground">
          No models found in the catalog. Selecting no models allows all models.
        </p>
      )}
      <form.Field name="expiresAt">
        {(field) => (
          <ExpirationField
            value={field.state.value}
            onChange={field.handleChange}
            disabled={busy}
          />
        )}
      </form.Field>
      <div className="flex flex-wrap gap-2">
        <Button
          type="button"
          variant="outline"
          disabled={busy}
          onClick={onCancel}
        >
          Cancel
        </Button>
        <Button type="submit" disabled={busy || optionsUnavailable}>
          {busy ? "Working…" : submitLabel}
        </Button>
      </div>
    </form>
  );
}
