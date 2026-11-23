import { useState, type ReactNode } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

function CodeBlock({ children, lang }: { children: string; lang?: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <div className="code-block">
      <div className="code-head">
        <span className="mono small muted">{lang ?? ""}</span>
        <button
          className="code-copy"
          onClick={() => {
            navigator.clipboard?.writeText(children).then(() => {
              setCopied(true);
              setTimeout(() => setCopied(false), 1200);
            });
          }}
        >
          {copied ? "복사됨" : "복사"}
        </button>
      </div>
      <pre>
        <code>{children}</code>
      </pre>
    </div>
  );
}

/** 에이전트 응답 마크다운 (표·체크리스트 포함, 코드 블록 복사) */
export function Markdown({ text }: { text: string }) {
  return (
    <div className="md">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={{
          // 코드 블록은 pre 안의 code로 온다. pre는 CodeBlock이 그리므로 그대로 통과시킨다.
          pre: ({ children }: { children?: ReactNode }) => <>{children}</>,
          code: ({ className, children }: { className?: string; children?: ReactNode }) => {
            const text = String(children ?? "");
            const lang = /language-(\w+)/.exec(className ?? "")?.[1];
            const block = lang != null || text.includes("\n");
            return block ? <CodeBlock lang={lang}>{text.replace(/\n$/, "")}</CodeBlock> : <code className="inline">{text}</code>;
          },
          a: ({ href, children }: { href?: string; children?: ReactNode }) => (
            <a href={href} target="_blank" rel="noreferrer">
              {children}
            </a>
          ),
        }}
      >
        {text}
      </ReactMarkdown>
    </div>
  );
}
