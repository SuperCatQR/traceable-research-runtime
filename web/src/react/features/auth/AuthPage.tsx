import { KeyRound, LoaderCircle } from "lucide-react";
import { useState } from "react";
import { useLocation, useNavigate } from "react-router-dom";
import { useAuthActions } from "../../data/workspace-actions";

function BrandMark() {
  return <span className="brand-mark" aria-hidden="true" />;
}

export function AuthPage() {
  const location = useLocation();
  const navigate = useNavigate();
  const auth = useAuthActions();
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [displayName, setDisplayName] = useState("");
  const isRegistering = location.pathname === "/register";

  return (
    <main className="authentication-screen">
      <section className="authentication-panel" aria-labelledby="authentication-title">
        <div className="authentication-brand">
          <BrandMark />
          <div>
            <p>Traceable Research</p>
            <span>source-grounded workspace</span>
          </div>
        </div>
        <div className="authentication-heading">
          <h1 id="authentication-title">{isRegistering ? "创建研究账户" : "返回研究工作区"}</h1>
        </div>
        <div className="authentication-tabs" role="tablist" aria-label="账户操作">
          <button type="button" role="tab" aria-selected={!isRegistering} onClick={() => { auth.clearError(); navigate("/login"); }}>
            登录
          </button>
          <button type="button" role="tab" aria-selected={isRegistering} onClick={() => { auth.clearError(); navigate("/register"); }}>
            注册
          </button>
        </div>
        <form
          className="stacked-form"
          onSubmit={(event) => {
            event.preventDefault();
            if (auth.pending) return;
            const input = isRegistering
              ? { kind: "register" as const, email: email.trim(), password, displayName: displayName.trim() }
              : { kind: "login" as const, email: email.trim(), password };
            void auth.submit(input).then((account) => {
              if (account) navigate("/research", { replace: true });
            });
          }}
        >
          {isRegistering && (
            <label>
              显示名称
              <input
                value={displayName}
                onChange={(event) => setDisplayName(event.target.value)}
                required
                maxLength={80}
                autoComplete="name"
              />
            </label>
          )}
          <label>
            邮箱
            <input
              value={email}
              onChange={(event) => setEmail(event.target.value)}
              type="email"
              required
              maxLength={320}
              autoComplete="email"
            />
          </label>
          <label>
            密码
            <input
              value={password}
              onChange={(event) => setPassword(event.target.value)}
              type="password"
              required
              minLength={12}
              maxLength={200}
              autoComplete={isRegistering ? "new-password" : "current-password"}
            />
          </label>
          {auth.error && <p className="inline-error" role="alert">{auth.error}</p>}
          <button className="primary-command" type="submit" disabled={auth.pending}>
            {auth.pending ? <LoaderCircle className="spin" aria-hidden="true" /> : <KeyRound aria-hidden="true" />}
            {isRegistering ? "创建账户" : "登录"}
          </button>
        </form>
      </section>
    </main>
  );
}
