import type { SVGProps } from "react";

type IconProps = SVGProps<SVGSVGElement> & { title?: string };

function base(
  props: IconProps,
  children: React.ReactNode,
  viewBox = "0 0 24 24",
) {
  const { title, ...rest } = props;
  return (
    <svg
      viewBox={viewBox}
      width="1em"
      height="1em"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.8}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden={title ? undefined : true}
      role={title ? "img" : undefined}
      focusable="false"
      {...rest}
    >
      {title ? <title>{title}</title> : null}
      {children}
    </svg>
  );
}

/* Accessible, consistent 24px line icons (decorative unless `title` given). */
export const Icon = {
  Home: (p: IconProps) =>
    base(
      p,
      <>
        <path d="M3 11.5 12 4l9 7.5" />
        <path d="M5 10v10h14V10" />
        <path d="M10 20v-6h4v6" />
      </>,
    ),
  Heart: (p: IconProps) =>
    base(
      p,
      <path d="M12 20s-7-4.5-7-10a4 4 0 0 1 7-2.5A4 4 0 0 1 19 10c0 5.5-7 10-7 10Z" />,
    ),
  Calendar: (p: IconProps) =>
    base(
      p,
      <>
        <rect x="3" y="5" width="18" height="16" rx="2" />
        <path d="M3 10h18M8 3v4M16 3v4" />
      </>,
    ),
  Tasks: (p: IconProps) =>
    base(
      p,
      <>
        <path d="m4 7 2 2 3-3" />
        <path d="M11 7h9" />
        <path d="m4 13 2 2 3-3" />
        <path d="M11 13h9" />
        <path d="m4 19 2 2 3-3" />
        <path d="M11 19h9" />
      </>,
    ),
  User: (p: IconProps) =>
    base(
      p,
      <>
        <circle cx="12" cy="8" r="4" />
        <path d="M4 21a8 8 0 0 1 16 0" />
      </>,
    ),
  Users: (p: IconProps) =>
    base(
      p,
      <>
        <circle cx="9" cy="8" r="3.5" />
        <path d="M2.5 20a6.5 6.5 0 0 1 13 0" />
        <path d="M16 4.5a3.5 3.5 0 0 1 0 7" />
        <path d="M17.5 13.5a6.5 6.5 0 0 1 4 6.5" />
      </>,
    ),
  Stethoscope: (p: IconProps) =>
    base(
      p,
      <>
        <path d="M6 3v6a5 5 0 0 0 10 0V3" />
        <path d="M11 14v2a5 5 0 0 0 10 0v-1" />
        <circle cx="21" cy="13" r="2" />
      </>,
    ),
  Flask: (p: IconProps) =>
    base(
      p,
      <>
        <path d="M9 3h6" />
        <path d="M10 3v6L4.5 19a1.5 1.5 0 0 0 1.3 2.2h12.4a1.5 1.5 0 0 0 1.3-2.2L14 9V3" />
        <path d="M7 15h10" />
      </>,
    ),
  Report: (p: IconProps) =>
    base(
      p,
      <>
        <path d="M7 3h7l5 5v13H7z" />
        <path d="M14 3v5h5" />
        <path d="M10 13h6M10 17h6" />
      </>,
    ),
  Pulse: (p: IconProps) => base(p, <path d="M3 12h4l2-6 4 12 2-6h6" />),
  Shield: (p: IconProps) =>
    base(
      p,
      <>
        <path d="M12 3 4.5 6v6c0 4.5 3.2 7.8 7.5 9 4.3-1.2 7.5-4.5 7.5-9V6Z" />
        <path d="m9 12 2 2 4-4" />
      </>,
    ),
  Door: (p: IconProps) =>
    base(
      p,
      <>
        <path d="M5 21V4a1 1 0 0 1 1-1h8v18" />
        <path d="M14 3h4a1 1 0 0 1 1 1v17" />
        <path d="M11 12h.01" />
        <path d="M3 21h18" />
      </>,
    ),
  Grid: (p: IconProps) =>
    base(
      p,
      <>
        <rect x="3" y="3" width="7" height="7" rx="1.5" />
        <rect x="14" y="3" width="7" height="7" rx="1.5" />
        <rect x="3" y="14" width="7" height="7" rx="1.5" />
        <rect x="14" y="14" width="7" height="7" rx="1.5" />
      </>,
    ),
  Clock: (p: IconProps) =>
    base(
      p,
      <>
        <circle cx="12" cy="12" r="9" />
        <path d="M12 7v5l3 2" />
      </>,
    ),
  Truck: (p: IconProps) =>
    base(
      p,
      <>
        <path d="M3 7h11v9H3z" />
        <path d="M14 10h4l3 3v3h-7" />
        <circle cx="7" cy="18" r="1.8" />
        <circle cx="17" cy="18" r="1.8" />
      </>,
    ),
  Layers: (p: IconProps) =>
    base(
      p,
      <>
        <path d="m12 3 9 5-9 5-9-5z" />
        <path d="m3 13 9 5 9-5" />
      </>,
    ),
  Bell: (p: IconProps) =>
    base(
      p,
      <>
        <path d="M6 16V11a6 6 0 0 1 12 0v5l1.5 2h-15Z" />
        <path d="M10 21a2 2 0 0 0 4 0" />
      </>,
    ),
  Search: (p: IconProps) =>
    base(
      p,
      <>
        <circle cx="11" cy="11" r="6.5" />
        <path d="m20 20-4-4" />
      </>,
    ),
  Close: (p: IconProps) => base(p, <path d="M6 6l12 12M18 6 6 18" />),
  Check: (p: IconProps) => base(p, <path d="m5 12 4.5 4.5L19 7" />),
  ChevronDown: (p: IconProps) => base(p, <path d="m6 9 6 6 6-6" />),
  ChevronRight: (p: IconProps) => base(p, <path d="m9 6 6 6-6 6" />),
  ChevronLeft: (p: IconProps) => base(p, <path d="m15 6-6 6 6 6" />),
  ArrowRight: (p: IconProps) => base(p, <path d="M4 12h16m-6-6 6 6-6 6" />),
  Plus: (p: IconProps) => base(p, <path d="M12 5v14M5 12h14" />),
  Menu: (p: IconProps) => base(p, <path d="M4 7h16M4 12h16M4 17h16" />),
  Collapse: (p: IconProps) =>
    base(
      p,
      <>
        <rect x="3" y="4" width="18" height="16" rx="2" />
        <path d="M9 4v16" />
        <path d="m15 10-2 2 2 2" />
      </>,
    ),
  Expand: (p: IconProps) =>
    base(
      p,
      <>
        <rect x="3" y="4" width="18" height="16" rx="2" />
        <path d="M9 4v16" />
        <path d="m14 10 2 2-2 2" />
      </>,
    ),
  Alert: (p: IconProps) =>
    base(
      p,
      <>
        <path d="M12 3 2.5 20h19Z" />
        <path d="M12 9v5M12 17h.01" />
      </>,
    ),
  Info: (p: IconProps) =>
    base(
      p,
      <>
        <circle cx="12" cy="12" r="9" />
        <path d="M12 11v5M12 8h.01" />
      </>,
    ),
  Mic: (p: IconProps) =>
    base(
      p,
      <>
        <rect x="9" y="3" width="6" height="11" rx="3" />
        <path d="M5 11a7 7 0 0 0 14 0" />
        <path d="M12 18v3" />
      </>,
    ),
  Pause: (p: IconProps) =>
    base(p, <path d="M8 5v14M16 5v14" strokeWidth={2.4} />),
  Play: (p: IconProps) =>
    base(p, <path d="M7 4v16l13-8z" fill="currentColor" />),
  Stop: (p: IconProps) =>
    base(
      p,
      <rect x="6" y="6" width="12" height="12" rx="2" fill="currentColor" />,
    ),
  Sparkle: (p: IconProps) =>
    base(
      p,
      <>
        <path d="M12 3v4M12 17v4M3 12h4M17 12h4" />
        <path
          d="m12 7 1.6 3.4L17 12l-3.4 1.6L12 17l-1.6-3.4L7 12l3.4-1.6Z"
          fill="currentColor"
          stroke="none"
        />
      </>,
    ),
  Globe: (p: IconProps) =>
    base(
      p,
      <>
        <circle cx="12" cy="12" r="9" />
        <path d="M3 12h18M12 3a14 14 0 0 1 0 18M12 3a14 14 0 0 0 0 18" />
      </>,
    ),
  Palette: (p: IconProps) =>
    base(
      p,
      <>
        <path d="M12 3a9 9 0 1 0 0 18h1.5a2 2 0 0 0 0-4H12a1.5 1.5 0 0 1 0-3h4.5A4.5 4.5 0 0 0 21 9.5C21 6 17 3 12 3Z" />
        <path d="M8 10h.01M12 7h.01M16 10h.01M8 14h.01" />
      </>,
    ),
  SignOut: (p: IconProps) =>
    base(
      p,
      <>
        <path d="M10 4H5v16h5" />
        <path d="M14 8l4 4-4 4M18 12H9" />
      </>,
    ),
  Drag: (p: IconProps) =>
    base(
      p,
      <>
        <circle cx="9" cy="6" r="1.2" fill="currentColor" />
        <circle cx="15" cy="6" r="1.2" fill="currentColor" />
        <circle cx="9" cy="12" r="1.2" fill="currentColor" />
        <circle cx="15" cy="12" r="1.2" fill="currentColor" />
        <circle cx="9" cy="18" r="1.2" fill="currentColor" />
        <circle cx="15" cy="18" r="1.2" fill="currentColor" />
      </>,
    ),
  Eye: (p: IconProps) =>
    base(
      p,
      <>
        <path d="M2 12s3.5-6 10-6 10 6 10 6-3.5 6-10 6S2 12 2 12Z" />
        <circle cx="12" cy="12" r="2.5" />
      </>,
    ),
  EyeOff: (p: IconProps) =>
    base(
      p,
      <>
        <path d="M3 3l18 18" />
        <path d="M10.6 6.3A10.6 10.6 0 0 1 12 6c6.5 0 10 6 10 6a16.6 16.6 0 0 1-3 3.6M6.4 6.8C3.6 8.7 2 12 2 12s3.5 6 10 6c1.3 0 2.5-.2 3.6-.6" />
      </>,
    ),
  Map: (p: IconProps) =>
    base(
      p,
      <>
        <path d="M12 21s6-5.5 6-11a6 6 0 0 0-12 0c0 5.5 6 11 6 11Z" />
        <circle cx="12" cy="10" r="2" />
      </>,
    ),
  Settings: (p: IconProps) =>
    base(
      p,
      <>
        <circle cx="12" cy="12" r="3" />
        <path d="M19 12a7 7 0 0 0-.1-1.2l2-1.5-2-3.4-2.4 1a7 7 0 0 0-2-1.2L14 3h-4l-.5 2.7a7 7 0 0 0-2 1.2l-2.4-1-2 3.4 2 1.5A7 7 0 0 0 5 12c0 .4 0 .8.1 1.2l-2 1.5 2 3.4 2.4-1a7 7 0 0 0 2 1.2L10 21h4l.5-2.7a7 7 0 0 0 2-1.2l2.4 1 2-3.4-2-1.5c.1-.4.1-.8.1-1.2Z" />
      </>,
    ),
  Wifi: (p: IconProps) =>
    base(
      p,
      <>
        <path d="M2 9a16 16 0 0 1 20 0" />
        <path d="M5.5 12.5a11 11 0 0 1 13 0" />
        <path d="M9 16a5.5 5.5 0 0 1 6 0" />
        <path d="M12 19.5h.01" />
      </>,
    ),
  Wand: (p: IconProps) =>
    base(
      p,
      <>
        <path d="m4 20 10-10" />
        <path
          d="m14 4 1 2 2 1-2 1-1 2-1-2-2-1 2-1z"
          fill="currentColor"
          stroke="none"
        />
        <path
          d="m18 12 .7 1.3L20 14l-1.3.7L18 16l-.7-1.3L16 14l1.3-.7z"
          fill="currentColor"
          stroke="none"
        />
      </>,
    ),
  Brain: (p: IconProps) =>
    base(
      p,
      <>
        <path d="M9 4a3 3 0 0 0-3 3 3 3 0 0 0-2 4 3 3 0 0 0 1 5.5A3 3 0 0 0 9 20c1.2 0 2.2-.6 3-1.5V5.5A3 3 0 0 0 9 4Z" />
        <path d="M15 4a3 3 0 0 1 3 3 3 3 0 0 1 2 4 3 3 0 0 1-1 5.5A3 3 0 0 1 15 20c-1.2 0-2.2-.6-3-1.5" />
        <path d="M12 9h3M9 13h3" />
      </>,
    ),
};

export type IconName = keyof typeof Icon;
