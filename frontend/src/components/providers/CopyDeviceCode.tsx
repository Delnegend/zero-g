import { Check } from "lucide-react";
import { useEffect, useState } from "react";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";

export function CopyDeviceCode({
  code,
  onError,
}: {
  code: string;
  onError: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [feedback, setFeedback] = useState<"idle" | "visible" | "fading">(
    "idle",
  );

  useEffect(() => {
    if (feedback !== "visible") return;
    const fade = window.setTimeout(() => setFeedback("fading"), 1000);
    return () => window.clearTimeout(fade);
  }, [feedback]);
  useEffect(() => {
    if (feedback !== "fading") return;
    const finish = window.setTimeout(() => setFeedback("idle"), 150);
    return () => window.clearTimeout(finish);
  }, [feedback]);

  async function copy() {
    if (feedback !== "idle") return;
    try {
      await navigator.clipboard.writeText(code);
      setFeedback("visible");
    } catch {
      onError();
    }
  }

  return (
    <Tooltip open={open || feedback !== "idle"} onOpenChange={setOpen}>
      <TooltipTrigger
        aria-label={`Copy device code ${code}`}
        aria-disabled={feedback !== "idle"}
        onClick={() => void copy()}
        className="relative w-fit overflow-hidden rounded px-1 text-left focus-visible:outline-2 focus-visible:outline-offset-2"
      >
        <code className="select-none text-lg font-semibold">{code}</code>
        <span
          aria-hidden="true"
          className={`pointer-events-none absolute inset-0 flex items-center justify-center rounded bg-green-100 text-green-700 transition-opacity duration-150 motion-reduce:transition-none dark:bg-green-950 dark:text-green-400 ${feedback === "visible" ? "opacity-100" : "opacity-0"}`}
        >
          <Check className="size-5" />
        </span>
        {feedback !== "idle" && (
          <span className="sr-only" role="status">
            Copied
          </span>
        )}
      </TooltipTrigger>
      <TooltipContent>
        {feedback === "idle" ? "Copy" : "Copied!"}
      </TooltipContent>
    </Tooltip>
  );
}
