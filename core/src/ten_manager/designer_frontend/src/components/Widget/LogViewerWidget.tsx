//
// Copyright © 2025 Agora
// This file is part of TEN Framework, an open source project.
// Licensed under the Apache License, Version 2.0, with certain conditions.
// Refer to the "LICENSE" file in the root directory for more information.
//
import * as React from "react";
import { LogViewer, LogViewerSearch } from "@patternfly/react-log-viewer";

import { cn } from "@/lib/utils";
import { useWidgetStore } from "@/store/widget";
import { ILogViewerWidget, ILogViewerWidgetOptions } from "@/types/widgets";
import { EWSMessageType } from "@/types/apps";

export function LogViewerBackstageWidget(props: ILogViewerWidget) {
  const {
    widget_id: id,
    metadata: { wsUrl, scriptType, script, postActions } = {},
  } = props;

  const { appendLogViewerHistory } = useWidgetStore();

  const wsRef = React.useRef<WebSocket | null>(null);

  React.useEffect(() => {
    if (!wsUrl || !scriptType || !script) {
      return;
    }

    wsRef.current = new WebSocket(wsUrl);

    wsRef.current.onopen = () => {
      console.log("[LogViewerWidget] WebSocket connected!");
      wsRef.current?.send(JSON.stringify(script));
    };

    wsRef.current.onmessage = (event) => {
      try {
        const msg = JSON.parse(event.data);

        if (
          msg.type === EWSMessageType.STANDARD_OUTPUT ||
          msg.type === EWSMessageType.STANDARD_ERROR
        ) {
          const line = msg.data;
          appendLogViewerHistory(id, [line]);
        } else if (msg.type === EWSMessageType.NORMAL_LINE) {
          const line = msg.data;
          appendLogViewerHistory(id, [line]);
        } else if (msg.type === EWSMessageType.EXIT) {
          const code = msg.code;
          const errMsg = msg?.error_message;
          appendLogViewerHistory(id, [
            errMsg,
            `Process exited with code ${code}. Closing...`,
          ]);

          wsRef.current?.close();
        } else if (msg.status === "fail") {
          appendLogViewerHistory(id, [
            `Error: ${msg.message || "Unknown error"}\n`,
          ]);
        } else {
          appendLogViewerHistory(id, [
            `Unknown message: ${JSON.stringify(msg)}`,
          ]);
        }
        // eslint-disable-next-line @typescript-eslint/no-unused-vars
      } catch (err) {
        // If it's not JSON, output it directly as text.
        appendLogViewerHistory(id, [event.data]);
      }
    };

    wsRef.current.onerror = (err) => {
      console.error("[LogViewerWidget] WebSocket error:", err);
    };

    wsRef.current.onclose = () => {
      console.log("[LogViewerWidget] WebSocket closed!");
      postActions?.();
    };

    return () => {
      // Close the connection when the component is unmounted.
      wsRef.current?.close();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id, wsUrl, scriptType, script]);

  return <></>;
}

export function LogViewerFrontStageWidget(props: {
  id: string;
  options?: ILogViewerWidgetOptions;
}) {
  const { id } = props;

  const { logViewerHistory } = useWidgetStore();

  const logsMemo = React.useMemo(() => {
    return logViewerHistory[id]?.history || [];
  }, [logViewerHistory, id]);

  return (
    <div
      className={cn(
        "flex h-full w-full flex-col",
        "[&_.pf-m-current]:!bg-ten-yellow-6",
        "[&_.pf-m-match]:!bg-ten-yellow-6/20"
      )}
      id={id}
    >
      <LogViewer
        hasLineNumbers={false}
        data={logsMemo}
        overScanCount={10}
        toolbar={
          <>
            <LogViewerSearch
              minSearchChars={3}
              placeholder="Search value"
              className={cn(
                "flex h-9 w-full px-3 py-1",
                "border border-input bg-transparent text-base",
                "placeholder:text-muted-foreground",
                "[&_input]:ml-6",
                "[&_input]:focus-visible:outline-hidden",
                "md:text-sm"
              )}
              style={{ borderRadius: "var(--radius-md)" }}
            />
          </>
        }
      />
    </div>
  );
}
