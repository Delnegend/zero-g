import { Check } from "lucide-react";
import { useEffect, useState } from "react";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";

/**
 * Text that copies itself on click, flashing a green check and a "Copied!" tooltip.
 * `fade` keeps the text on one line and fades its end out instead of wrapping.
 */
export function CopyText({
  text,
  label,
  fade = false,
  textClassName = "",
  onError,
}: {
  text: string;
  label: string;
  fade?: boolean;
  textClassName?: string;
  onError: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [feedback, setFeedback] = useState<"idle" | "visible" | "fading">(
    "idle",
  );

  useEffect(() => {
    if (feedback !== "visible") return;
    const fadeOut = window.setTimeout(() => setFeedback("fading"), 1000);
    return () => window.clearTimeout(fadeOut);
  }, [feedback]);
  useEffect(() => {
    if (feedback !== "fading") return;
    const finish = window.setTimeout(() => setFeedback("idle"), 150);
    return () => window.clearTimeout(finish);
  }, [feedback]);

  async function copy() {
    if (feedback !== "idle") return;
    try {
      await navigator.clipboard.writeText(text);
      setFeedback("visible");
    } catch {
      onError();
    }
  }

  return (
    <Tooltip open={open || feedback !== "idle"} onOpenChange={setOpen}>
      <TooltipTrigger
        aria-label={label}
        aria-disabled={feedback !== "idle"}
        onClick={() => void copy()}
        className={`relative overflow-hidden rounded px-1 text-left focus-visible:outline-2 focus-visible:outline-offset-2 ${fade ? "block w-full" : "w-fit"}`}
      >
        <code
          className={`select-none ${fade ? "block overflow-hidden [mask-image:linear-gradient(to_right,black_70%,transparent)] whitespace-nowrap" : ""} ${textClassName}`}
        >
          {text}
        </code>
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
        {feedback === "idle" ? "Click to copy" : "Copied!"}
      </TooltipContent>
    </Tooltip>
  );
}
