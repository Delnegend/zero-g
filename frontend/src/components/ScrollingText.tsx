import { useEffect, useRef, useState, type CSSProperties } from "react";

export function ScrollingText({
  text,
  align = "left",
}: {
  text: string;
  align?: "left" | "right";
}) {
  const container = useRef<HTMLSpanElement>(null);
  const content = useRef<HTMLSpanElement>(null);
  const [size, setSize] = useState({ width: 0, overflow: false });
  const [visible, setVisible] = useState(false);

  useEffect(() => {
    const outer = container.current;
    const inner = content.current;
    if (!outer || !inner) return;
    const measure = () => {
      const width = inner.offsetWidth;
      const overflow = width > outer.clientWidth;
      setSize((previous) =>
        previous.width === width && previous.overflow === overflow
          ? previous
          : { width, overflow },
      );
    };
    const resize = new ResizeObserver(measure);
    resize.observe(outer);
    resize.observe(inner);
    measure();
    const intersection = new IntersectionObserver(([entry]) =>
      setVisible(entry.isIntersecting),
    );
    intersection.observe(outer);
    return () => {
      resize.disconnect();
      intersection.disconnect();
    };
  }, []);

  return (
    <span
      ref={container}
      title={size.overflow ? text : undefined}
      className={`scrolling-text relative block min-w-0 overflow-hidden whitespace-nowrap ${size.overflow ? "scrolling-text-overflow text-left" : align === "right" ? "text-right" : "text-left"}`}
      data-scrolling={size.overflow && visible}
      style={
        {
          "--scroll-distance": `${size.width + 48}px`,
          "--scroll-duration": `${Math.max(4, (size.width + 48) / 35 + 3)}s`,
        } as CSSProperties
      }
    >
      <span className="scrolling-text-content inline-flex w-max">
        <span ref={content}>{text}</span>
        {size.overflow && <span aria-hidden="true">{text}</span>}
      </span>
      {size.overflow && (
        <>
          <span aria-hidden="true" className="scrolling-text-fade-left" />
          <span aria-hidden="true" className="scrolling-text-fade-right" />
        </>
      )}
    </span>
  );
}
