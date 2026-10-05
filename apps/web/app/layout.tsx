import type { Metadata, Viewport } from "next";
import localFont from "next/font/local";
import "./globals.css";
import { SessionProvider } from "@/lib/session";

const inter = localFont({
  src: [
    { path: "./fonts/inter-latin-wght-normal.woff2", style: "normal" },
    { path: "./fonts/inter-latin-ext-wght-normal.woff2", style: "normal" },
  ],
  variable: "--font-inter",
  display: "swap",
  weight: "100 900",
});

export const metadata: Metadata = {
  title: "WellOS",
  description:
    "WellOS — AI-native clinical platform with accountable human decisions",
};

export const viewport: Viewport = {
  width: "device-width",
  initialScale: 1,
  viewportFit: "cover",
  themeColor: "#0b1526",
};

export default function RootLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <html lang="en" data-theme="north" className={inter.variable}>
      <body>
        <SessionProvider>{children}</SessionProvider>
      </body>
    </html>
  );
}
