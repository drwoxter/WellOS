import type { Metadata } from "next";
import "./globals.css";
import { SessionProvider } from "@/lib/session";

export const metadata: Metadata = {
  title: "WellOS",
  description: "WellOS clinician workspace (development)",
};

export default function RootLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <html lang="en" data-theme="north">
      <body>
        <SessionProvider>{children}</SessionProvider>
      </body>
    </html>
  );
}
