import * as React from "react";
import { cn } from "cn";
import { Button } from "@/components/ui/button";
import { ChevronLeftIcon, ChevronRightIcon } from "lucide-react";

type CarouselContextProps = {
  carouselRef: React.RefObject<HTMLDivElement | null>;
  scrollPrev: () => void;
  scrollNext: () => void;
  canScrollPrev: boolean;
  canScrollNext: boolean;
  orientation?: "horizontal" | "vertical";
};

const CarouselContext = React.createContext<CarouselContextProps | null>(null);

function useCarousel() {
  const context = React.useContext(CarouselContext);
  if (!context) {
    throw new Error("useCarousel must be used within a <Carousel />");
  }
  return context;
}

type CarouselProps = {
  orientation?: "horizontal" | "vertical";
  opts?: Record<string, unknown>;
  setApi?: (api: unknown) => void;
  plugins?: unknown;
};

function Carousel({
  orientation = "horizontal",
  className,
  children,
  ...props
}: React.ComponentProps<"div"> & CarouselProps) {
  const carouselRef = React.useRef<HTMLDivElement>(null);
  const [canScrollPrev, setCanScrollPrev] = React.useState(false);
  const [canScrollNext, setCanScrollNext] = React.useState(false);

  const updateScrollState = React.useCallback(() => {
    const el = carouselRef.current;
    if (!el) return;
    if (orientation === "horizontal") {
      setCanScrollPrev(el.scrollLeft > 1);
      setCanScrollNext(el.scrollLeft + el.clientWidth < el.scrollWidth - 1);
    } else {
      setCanScrollPrev(el.scrollTop > 1);
      setCanScrollNext(el.scrollTop + el.clientHeight < el.scrollHeight - 1);
    }
  }, [orientation]);

  React.useEffect(() => {
    const el = carouselRef.current;
    if (!el) return;
    updateScrollState();
    el.addEventListener("scroll", updateScrollState, { passive: true });
    const resizeObserver = new ResizeObserver(updateScrollState);
    resizeObserver.observe(el);
    return () => {
      el.removeEventListener("scroll", updateScrollState);
      resizeObserver.disconnect();
    };
  }, [updateScrollState]);

  const scrollPrev = React.useCallback(() => {
    const el = carouselRef.current;
    if (!el) return;
    if (orientation === "horizontal") {
      el.scrollBy({ left: -el.clientWidth, behavior: "smooth" });
    } else {
      el.scrollBy({ top: -el.clientHeight, behavior: "smooth" });
    }
  }, [orientation]);

  const scrollNext = React.useCallback(() => {
    const el = carouselRef.current;
    if (!el) return;
    if (orientation === "horizontal") {
      el.scrollBy({ left: el.clientWidth, behavior: "smooth" });
    } else {
      el.scrollBy({ top: el.clientHeight, behavior: "smooth" });
    }
  }, [orientation]);

  const handleKeyDown = React.useCallback(
    (event: React.KeyboardEvent<HTMLDivElement>) => {
      if (event.key === "ArrowLeft") {
        event.preventDefault();
        scrollPrev();
      } else if (event.key === "ArrowRight") {
        event.preventDefault();
        scrollNext();
      }
    },
    [scrollPrev, scrollNext],
  );

  return (
    <CarouselContext.Provider
      value={{
        carouselRef,
        orientation,
        scrollPrev,
        scrollNext,
        canScrollPrev,
        canScrollNext,
      }}
    >
      <div
        onKeyDownCapture={handleKeyDown}
        className={cn("relative", className)}
        role="region"
        aria-roledescription="carousel"
        data-slot="carousel"
        {...props}
      >
        {children}
      </div>
    </CarouselContext.Provider>
  );
}

function CarouselContent({ className, ...props }: React.ComponentProps<"div">) {
  const { carouselRef, orientation } = useCarousel();

  return (
    <div
      ref={carouselRef}
      className={cn(
        "snap-x snap-mandatory scrollbar-none overflow-x-auto scroll-smooth [&::-webkit-scrollbar]:hidden",
        orientation === "vertical" &&
          "snap-y overflow-x-hidden overflow-y-auto",
      )}
      data-slot="carousel-content"
    >
      <div
        className={cn(
          "flex",
          orientation === "horizontal" ? "-ml-4" : "-mt-4 flex-col",
          className,
        )}
        {...props}
      />
    </div>
  );
}

function CarouselItem({ className, ...props }: React.ComponentProps<"div">) {
  const { orientation } = useCarousel();

  return (
    <div
      role="group"
      aria-roledescription="slide"
      data-slot="carousel-item"
      className={cn(
        "min-w-0 shrink-0 grow-0 basis-full snap-start",
        orientation === "horizontal" ? "pl-4" : "pt-4",
        className,
      )}
      {...props}
    />
  );
}

function CarouselPrevious({
  className,
  variant = "outline",
  size = "icon-sm",
  ...props
}: React.ComponentProps<typeof Button>) {
  const { orientation, scrollPrev, canScrollPrev } = useCarousel();

  return (
    <Button
      data-slot="carousel-previous"
      variant={variant}
      size={size}
      className={cn(
        "absolute touch-manipulation rounded-full",
        orientation === "horizontal"
          ? "inset-y-0 -left-12 my-auto"
          : "-top-12 left-1/2 -translate-x-1/2 rotate-90",
        className,
      )}
      disabled={!canScrollPrev}
      onClick={scrollPrev}
      {...props}
    >
      <ChevronLeftIcon />
      <span className="sr-only">Previous slide</span>
    </Button>
  );
}

function CarouselNext({
  className,
  variant = "outline",
  size = "icon-sm",
  ...props
}: React.ComponentProps<typeof Button>) {
  const { orientation, scrollNext, canScrollNext } = useCarousel();

  return (
    <Button
      data-slot="carousel-next"
      variant={variant}
      size={size}
      className={cn(
        "absolute touch-manipulation rounded-full",
        orientation === "horizontal"
          ? "inset-y-0 -right-12 my-auto"
          : "-bottom-12 left-1/2 -translate-x-1/2 rotate-90",
        className,
      )}
      disabled={!canScrollNext}
      onClick={scrollNext}
      {...props}
    >
      <ChevronRightIcon />
      <span className="sr-only">Next slide</span>
    </Button>
  );
}

export {
  type CarouselProps,
  Carousel,
  CarouselContent,
  CarouselItem,
  CarouselPrevious,
  CarouselNext,
  useCarousel,
};
