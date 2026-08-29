/** @type {import('next').NextConfig} */
const nextConfig = {
  reactStrictMode: true,
  async rewrites() {
    const api = process.env.WELLOS_API_URL ?? "http://localhost:8080";
    return [
      { source: "/api/:path*", destination: `${api}/api/:path*` },
      { source: "/fhir/:path*", destination: `${api}/fhir/:path*` },
    ];
  },
};

export default nextConfig;
