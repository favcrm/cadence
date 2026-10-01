import { useRef } from "react";
import { api } from "../../lib/api";
import Button from "../../ui/Button";
import { type IssuePageProps as Props } from "./issuePageProps";

export function Attach({
  id,
  onWrite,
  onError,
}: {
  id: string;
  onWrite: Props["onWrite"];
  onError: Props["onError"];
}) {
  const input = useRef<HTMLInputElement>(null);
  return (
    <>
      <Button onClick={() => input.current?.click()}>Attach</Button>
      <input
        ref={input}
        type="file"
        multiple
        className="hidden"
        onChange={(e) => {
          const list = e.target.files;
          if (!list) return;
          for (const f of Array.from(list)) {
            api
              .attach(id, f.name, f)
              .then((r) => onWrite(r, `${id} attach ${f.name}`))
              .catch((err) => onError(err, `attach ${f.name}`));
          }
          e.target.value = "";
        }}
      />
    </>
  );
}
