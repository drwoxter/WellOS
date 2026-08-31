import { NextRequest } from "next/server";
import { proxyToApi } from "@/lib/bff";

export const dynamic = "force-dynamic";

type Params = { params: { path: string[] } };

export async function GET(req: NextRequest, { params }: Params) {
  return proxyToApi(req, `/api/v1/${params.path.join("/")}`);
}

export async function POST(req: NextRequest, { params }: Params) {
  return proxyToApi(req, `/api/v1/${params.path.join("/")}`);
}
