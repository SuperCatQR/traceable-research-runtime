const DEFAULT_API_PROXY_TARGET = "http://127.0.0.1:8080";

export function selectApiProxyTarget(
  shellValue: string | undefined,
  fileValue: string | undefined,
): string {
  return shellValue?.trim() || fileValue?.trim() || DEFAULT_API_PROXY_TARGET;
}
