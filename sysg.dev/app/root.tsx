import { ChakraProvider } from "@chakra-ui/react";
import { Links, Meta, Outlet, Scripts, ScrollRestoration } from "react-router";
import { Footer } from "~/ds/Footer";
import { Navbar } from "~/ds/Navbar";
import { system } from "~/ds/system";
import { themeInitScript } from "~/ds/theme";
import "~/ds/tokens.css";

export function meta() {
  return [
    { title: "systemg" },
    { name: "description", content: "An agent-friendly general process composer." },
    { property: "og:title", content: "systemg" },
    { property: "og:description", content: "An agent-friendly general process composer." },
    { property: "og:type", content: "website" },
    { property: "og:url", content: "https://sysg.dev" },
    { property: "og:image", content: "https://sysg.dev/og.png" },
    { name: "twitter:card", content: "summary_large_image" },
    { name: "twitter:title", content: "systemg" },
    { name: "twitter:description", content: "An agent-friendly general process composer." },
    { name: "twitter:image", content: "https://sysg.dev/og.png" },
  ];
}

export function Layout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en" data-theme="system" suppressHydrationWarning>
      <head>
        <meta charSet="utf-8" />
        <meta name="viewport" content="width=device-width, initial-scale=1" />
        <link rel="icon" href="/logo-mark.svg" type="image/svg+xml" />
        <link rel="icon" href="/icon-32.png" sizes="32x32" type="image/png" />
        <link rel="apple-touch-icon" href="/icon-180.png" sizes="180x180" />
        <link
          rel="preload"
          as="font"
          type="font/woff2"
          crossOrigin="anonymous"
          href="/fonts/SpaceGrotesk-normal-latin.woff2"
        />
        <link
          rel="preload"
          as="font"
          type="font/woff2"
          crossOrigin="anonymous"
          href="/fonts/JetBrainsMono-normal-latin.woff2"
        />
        <Meta />
        <Links />
        <script dangerouslySetInnerHTML={{ __html: themeInitScript }} />
      </head>
      <body suppressHydrationWarning>
        <ChakraProvider value={system}>{children}</ChakraProvider>
        <ScrollRestoration />
        <Scripts />
      </body>
    </html>
  );
}

export default function App() {
  return (
    <>
      <Navbar />
      <Outlet />
      <Footer />
    </>
  );
}
