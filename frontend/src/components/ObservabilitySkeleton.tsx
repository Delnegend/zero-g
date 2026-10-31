import { Card } from "./ui/card";
import { Skeleton } from "./ui/skeleton";

function LogsSkeleton() {
  return (
    <Card className="min-w-0 py-0">
      <div className="flex flex-wrap items-end justify-between gap-4 border-b p-5">
        <div>
          <h2 className="text-lg font-semibold">Request logs</h2>
          <p className="mt-1 text-sm text-muted-foreground">
            The latest 200 matching requests. Select one to inspect its details.
          </p>
        </div>
        <Skeleton className="h-9 w-full sm:w-72" />
      </div>
      <div className="px-5 pt-4 sm:hidden">
        <div className="flex items-center gap-3 text-sm">
          <span>Sort by</span>
          <Skeleton className="h-9 w-36" />
        </div>
      </div>
      <div className="sm:hidden">
        {Array.from({ length: 3 }, (_, index) => (
          <div
            key={index}
            className="relative min-w-0 border-b p-4 pl-6 last:border-0"
          >
            <Skeleton className="absolute top-4 bottom-4 left-3 w-1" />
            <div className="flex items-start justify-between gap-3">
              <div className="space-y-1">
                <Skeleton className="h-5 w-32" />
                <Skeleton className="h-4 w-20" />
              </div>
              <Skeleton className="h-8 w-14 shrink-0" />
            </div>
            <Skeleton className="mt-3 h-6 w-20" />
            <div className="mt-3 flex flex-wrap items-center gap-x-3 gap-y-1">
              <div className="space-y-1">
                <Skeleton className="h-3 w-28" />
                <Skeleton className="h-3 w-16" />
              </div>
              <Skeleton className="h-6 w-10" />
              <Skeleton className="h-3 w-12" />
              <Skeleton className="h-3 w-16" />
            </div>
          </div>
        ))}
      </div>
      <div className="hidden min-w-0 overflow-x-auto sm:block">
        <table className="w-full min-w-[850px] text-left text-sm">
          <thead className="border-b">
            <tr>
              <th className="w-1 px-3 py-3" />
              {["w-16", "w-32", "w-14", "w-16", "w-16", "w-16"].map(
                (width, index) => (
                  <th key={index} className="px-5 py-3">
                    <Skeleton className={`h-8 ${width}`} />
                  </th>
                ),
              )}
            </tr>
          </thead>
          <tbody>
            {Array.from({ length: 3 }, (_, index) => (
              <tr key={index} className="border-b last:border-0">
                <td className="w-1 px-3 py-1.5">
                  <Skeleton className="h-6 w-1" />
                </td>
                <td className="px-4 py-1.5">
                  <div className="space-y-1">
                    <Skeleton className="h-3 w-28" />
                    <Skeleton className="h-3 w-16" />
                  </div>
                </td>
                <td className="px-4 py-1.5">
                  <div className="space-y-1">
                    <Skeleton className="h-4 w-28" />
                    <Skeleton className="h-3 w-16" />
                  </div>
                </td>
                <td className="px-4 py-1.5">
                  <Skeleton className="h-6 w-16" />
                </td>
                <td className="px-4 py-1.5">
                  <Skeleton className="h-6 w-10" />
                </td>
                <td className="px-4 py-1.5">
                  <Skeleton className="h-4 w-14" />
                </td>
                <td className="px-4 py-1.5">
                  <Skeleton className="h-3 w-16" />
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      <div className="flex flex-wrap items-center justify-between gap-3 border-t px-5 py-4">
        <Skeleton className="h-5 w-32" />
        <div className="flex items-center gap-2">
          <Skeleton className="h-8 w-20" />
          <Skeleton className="h-5 w-10" />
          <Skeleton className="h-8 w-16" />
        </div>
      </div>
    </Card>
  );
}

export function ObservabilitySkeleton({ logsOnly }: { logsOnly: boolean }) {
  return (
    <div
      role="status"
      aria-label={logsOnly ? "Loading request logs" : "Loading dashboard"}
    >
      <div aria-hidden="true">
        {logsOnly ? (
          <LogsSkeleton />
        ) : (
          <>
            <div className="mb-3 grid min-w-0 grid-cols-2 gap-3 @min-[1008px]:grid-cols-4">
              {Array.from({ length: 4 }, (_, index) => (
                <Card key={index} className="min-w-0 gap-0 p-3 sm:p-5">
                  <Skeleton className="h-5 w-28" />
                  <Skeleton className="mt-4 h-8 w-24" />
                  <Skeleton className="mt-1 h-4 w-36 max-w-full" />
                </Card>
              ))}
            </div>
            <div className="grid min-w-0 grid-cols-1 gap-3 @min-[672px]:grid-cols-2">
              {Array.from({ length: 6 }, (_, index) => (
                <Card
                  key={index}
                  className="flex h-[207px] min-w-0 flex-col gap-0 p-3 shadow-none sm:h-[293px] sm:p-5"
                >
                  <div className="flex min-h-8 items-start justify-between gap-2">
                    <Skeleton className="h-5 w-40 max-w-[70%]" />
                    {index !== 4 && <Skeleton className="h-4 w-12" />}
                  </div>
                  <Skeleton className="min-h-0 w-full flex-1" />
                  <Skeleton className="mt-1 h-3 w-32" />
                </Card>
              ))}
            </div>
          </>
        )}
      </div>
    </div>
  );
}
