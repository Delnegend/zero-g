import { RefreshCw } from "lucide-react";
import { Button } from "./ui/button";

export function RefreshButton({
  loading,
  onClick,
}: {
  loading: boolean;
  onClick: () => void;
}) {
  return (
    <Button
      type="button"
      variant="outline"
      className="w-28"
      onClick={onClick}
      disabled={loading}
    >
      <RefreshCw
        aria-hidden="true"
        className={loading ? "animate-spin motion-reduce:animate-none" : ""}
      />
      {loading ? "Updating" : "Refresh"}
    </Button>
  );
}
