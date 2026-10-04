import { NextRequest } from "next/server";
import { proxyToApi } from "@/lib/bff";

export const dynamic = "force-dynamic";

type Params = { params: Promise<{ path: string[] }> };

async function proxy(req: NextRequest, { params }: Params) {
  const { path } = await params;
  return proxyToApi(req, `/api/v1/${path.join("/")}`);
}

export const GET = proxy;
export const POST = proxy;
export const PUT = proxy;
export const PATCH = proxy;
export const DELETE = proxy;
