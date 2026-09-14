import { NextRequest } from "next/server";
import { proxyToApi } from "@/lib/bff";

export const dynamic = "force-dynamic";

type Params = { params: Promise<{ path: string[] }> };

export async function GET(req: NextRequest, { params }: Params) {
  const { path } = await params;
  return proxyToApi(req, `/api/v1/${path.join("/")}`);
}

export async function POST(req: NextRequest, { params }: Params) {
  const { path } = await params;
  return proxyToApi(req, `/api/v1/${path.join("/")}`);
}
