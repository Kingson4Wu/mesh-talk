import { useEffect, useState } from "react";
import { save } from "@tauri-apps/plugin-dialog";
import { Download, FileDown, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { chat } from "@/lib/api";
import { errorMessage } from "@/lib/error";
import { humanSize } from "@/lib/format";
import { useChat } from "@/store/chat";

const IMAGE_EXT = /\.(png|jpe?g|gif|webp|bmp|avif)$/i;
const isImage = (name: string) => IMAGE_EXT.test(name);

/** Inline thumbnail for a received image: fetch the decrypted bytes and show them via a
 * short-lived object URL (revoked on unmount). Renders nothing until/unless it loads. */
function ImageThumb({ fileConv, alt }: { fileConv: string; alt: string }) {
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    let alive = true;
    let objectUrl: string | null = null;
    chat
      .readFile(fileConv)
      .then((buf) => {
        if (!alive) return;
        objectUrl = URL.createObjectURL(new Blob([buf]));
        setUrl(objectUrl);
      })
      .catch(() => {});
    return () => {
      alive = false;
      if (objectUrl) URL.revokeObjectURL(objectUrl);
    };
  }, [fileConv]);
  if (!url) return null;
  return (
    <img
      src={url}
      alt={alt}
      className="mb-1.5 max-h-44 w-full rounded-lg object-cover"
    />
  );
}

export function FilesTray() {
  const files = useChat((s) => s.incomingFiles);
  const dismissFile = useChat((s) => s.dismissFile);
  const saveFile = useChat((s) => s.saveFile);
  const setError = useChat((s) => s.setError);

  const saveOne = async (fileConv: string, name: string) => {
    try {
      const dest = await save({ defaultPath: name });
      if (typeof dest === "string") await saveFile(fileConv, dest);
    } catch (e) {
      setError(`Couldn't save file: ${errorMessage(e)}`);
    }
  };

  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          title="Received files"
          className="relative"
        >
          <Download className="h-4 w-4" />
          {files.length > 0 && (
            <Badge className="absolute -right-0.5 -top-0.5 h-4 min-w-4 justify-center bg-primary px-1 text-primary-foreground">
              {files.length}
            </Badge>
          )}
        </Button>
      </PopoverTrigger>
      <PopoverContent align="end" className="w-72 p-0">
        <div className="border-b px-3 py-2 text-sm font-semibold">
          Received files
        </div>
        {files.length === 0 ? (
          <p className="px-3 py-4 text-center text-sm text-muted-foreground">
            Nothing received yet.
          </p>
        ) : (
          <div className="max-h-72 overflow-y-auto p-1">
            {files.map((f) => (
              <div
                key={f.fileConv}
                className="rounded-lg px-2 py-1.5 hover:bg-accent/50"
              >
                {isImage(f.name) && (
                  <ImageThumb fileConv={f.fileConv} alt={f.name} />
                )}
                <div className="flex items-center gap-2">
                  <FileDown className="h-4 w-4 shrink-0 text-muted-foreground" />
                  <div className="min-w-0 flex-1">
                    <div className="truncate text-sm">{f.name}</div>
                    <div className="truncate text-xs text-muted-foreground">
                      {f.fromName} · {humanSize(f.size)}
                    </div>
                  </div>
                  <Button
                    size="sm"
                    variant="secondary"
                    className="h-7"
                    onClick={() => saveOne(f.fileConv, f.name)}
                  >
                    Save
                  </Button>
                  <button
                    onClick={() => dismissFile(f.fileConv)}
                    className="rounded p-1 text-muted-foreground hover:bg-accent"
                  >
                    <X className="h-3.5 w-3.5" />
                  </button>
                </div>
              </div>
            ))}
          </div>
        )}
      </PopoverContent>
    </Popover>
  );
}
