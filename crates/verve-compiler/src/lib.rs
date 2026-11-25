//! Immutable layered configuration compilation pipeline (`CompilerStage`, YAML Merge, and `ScriptEngine`).

mod merge;
mod model;
mod pipeline;
mod script;
mod stages;

pub use model::{
    CommittedConfig, CompileContext, CompileDiagnostic, CompiledConfig, RuntimeInjectionConfig,
    StageLog,
};
pub use pipeline::CompilerPipeline;
pub use script::{NoopScriptEngine, SandboxLimits, ScriptEngine, ScriptOutput};
pub use stages::{
    CompilerStage, GlobalMergeStage, GlobalScriptStage, ProfileMergeStage, ProfileScriptStage,
    QuickRulesStage, RuntimeInjectionStage,
};

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::path::PathBuf;
    use std::pin::pin;
    use std::sync::Arc;
    use std::task::{Context, Poll, Waker};

    use serde_json::{Value, json};
    use verve_kernel::{ConfigDiagnostic, ControlEndpoint, LogLevel, ProxyMode};

    use super::*;

    fn block_on<F: Future>(fut: F) -> F::Output {
        let mut fut = pin!(fut);
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        loop {
            match fut.as_mut().poll(&mut cx) {
                Poll::Ready(val) => return val,
                Poll::Pending => std::thread::yield_now(),
            }
        }
    }

    fn base_context() -> CompileContext {
        CompileContext {
            profile_name: "work".into(),
            profile_merge_yaml: None,
            profile_script: None,
            global_merge_yaml: None,
            global_script: None,
            quick_rules: Vec::new(),
            runtime: RuntimeInjectionConfig {
                mixed_port: 7897,
                allow_lan: false,
                ipv6: false,
                mode: ProxyMode::Rule,
                log_level: LogLevel::Info,
                control_endpoint: ControlEndpoint::NamedPipe(r"\\.\pipe\verve-test".into()),
                tun_enabled: false,
            },
        }
    }

    #[test]
    fn invalid_yaml_reports_line_and_column_diagnostic() {
        let pipeline = CompilerPipeline::default();
        let bad_yaml = "mixed-port: 7890\nproxies:\n  - name: [unclosed\n";

        let err = pipeline
            .compile_to_yaml(bad_yaml, &base_context())
            .expect_err("invalid YAML syntax must return diagnostic");
        assert_eq!(err.stage, "parse");
        assert!(err.line.is_some());
        assert!(err.column.is_some());
        assert!(!err.message.is_empty());

        let mut ctx = base_context();
        ctx.global_merge_yaml = Some("dns:\n  enable: [broken\n".into());
        let merge_err = pipeline
            .compile_to_yaml("mixed-port: 7890\n", &ctx)
            .expect_err("invalid merge YAML must return diagnostic");
        assert_eq!(merge_err.stage, "global-merge");
        assert!(merge_err.line.is_some());
        assert!(merge_err.column.is_some());
    }

    #[test]
    fn deep_merge_and_all_six_list_directives_preserve_order_and_strip_directives() {
        let raw_profile = r#"
external-controller: 127.0.0.1:9090
dns:
  enable: false
  enhanced-mode: redir-host
  nameserver:
    - 114.114.114.114
proxies:
  - name: base-node
    type: ss
proxy-groups:
  - name: BaseGroup
    type: select
    proxies:
      - base-node
rules:
  - DOMAIN,base.example,BaseGroup
  - MATCH,DIRECT
"#;

        let profile_merge = r#"
dns:
  enable: true
  fallback:
    - 1.1.1.1
prepend-rules:
  - DOMAIN,prof-pre.example,BaseGroup
append-rules:
  - DOMAIN,prof-app.example,DIRECT
prepend-proxies:
  - name: prof-pre-node
    type: vless
append-proxies:
  - name: prof-app-node
    type: trojan
prepend-proxy-groups:
  - name: ProfPreGroup
    type: select
append-proxy-groups:
  - name: ProfAppGroup
    type: fallback
"#;

        let global_merge = r#"
dns:
  ipv6: true
prepend-rules:
  - DOMAIN,glob-pre.example,BaseGroup
append-rules:
  - DOMAIN,glob-app.example,DIRECT
"#;

        let mut ctx = base_context();
        ctx.profile_merge_yaml = Some(profile_merge.into());
        ctx.global_merge_yaml = Some(global_merge.into());

        let compiled = CompilerPipeline::default()
            .compile_to_yaml(raw_profile, &ctx)
            .expect("compilation must succeed");

        let doc = &compiled.document;
        // Deep merge preserves untouched keys (`enhanced-mode`) while overlaying `enable` and `ipv6`.
        assert_eq!(doc["dns"]["enable"], Value::Bool(true));
        assert_eq!(doc["dns"]["enhanced-mode"], json!("redir-host"));
        assert_eq!(doc["dns"]["ipv6"], Value::Bool(true));
        assert_eq!(doc["dns"]["fallback"], json!(["1.1.1.1"]));

        // Directive keys must be stripped from final output.
        for key in [
            "prepend-rules",
            "append-rules",
            "prepend-proxies",
            "append-proxies",
            "prepend-proxy-groups",
            "append-proxy-groups",
        ] {
            assert!(
                doc.get(key).is_none(),
                "directive key `{key}` must be stripped"
            );
            assert!(
                !compiled.yaml.contains(key),
                "rendered YAML must not contain `{key}`"
            );
        }

        // Rules order: global-prepend -> profile-prepend -> base -> profile-append -> global-append.
        let rules: Vec<&str> = doc["rules"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(
            rules,
            vec![
                "DOMAIN,glob-pre.example,BaseGroup",
                "DOMAIN,prof-pre.example,BaseGroup",
                "DOMAIN,base.example,BaseGroup",
                "MATCH,DIRECT",
                "DOMAIN,prof-app.example,DIRECT",
                "DOMAIN,glob-app.example,DIRECT",
            ]
        );

        let proxy_names: Vec<&str> = doc["proxies"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            proxy_names,
            vec!["prof-pre-node", "base-node", "prof-app-node"]
        );

        let group_names: Vec<&str> = doc["proxy-groups"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            group_names,
            vec!["ProfPreGroup", "BaseGroup", "ProfAppGroup"]
        );
    }

    #[test]
    fn quick_rules_prepend_and_runtime_injection_configures_pipe_or_uds_and_tun() {
        let raw_profile = r#"
external-controller: 0.0.0.0:9090
external-controller-unix: /tmp/stale.sock
rules:
  - MATCH,DIRECT
"#;

        let mut ctx = base_context();
        ctx.profile_merge_yaml = Some("prepend-rules:\n  - DOMAIN,merged.example,Proxy\n".into());
        ctx.quick_rules = vec![
            "DOMAIN-SUFFIX,quick1.example,Proxy".into(),
            "PROCESS-NAME,curl,DIRECT".into(),
        ];
        ctx.runtime = RuntimeInjectionConfig {
            mixed_port: 17897,
            allow_lan: true,
            ipv6: true,
            mode: ProxyMode::Global,
            log_level: LogLevel::Debug,
            control_endpoint: ControlEndpoint::NamedPipe(r"\\.\pipe\verve-mihomo-main".into()),
            tun_enabled: true,
        };

        let compiled = CompilerPipeline::default()
            .compile_to_yaml(raw_profile, &ctx)
            .unwrap();
        let doc = &compiled.document;

        let rules: Vec<&str> = doc["rules"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(
            rules,
            vec![
                "DOMAIN-SUFFIX,quick1.example,Proxy",
                "PROCESS-NAME,curl,DIRECT",
                "DOMAIN,merged.example,Proxy",
                "MATCH,DIRECT",
            ]
        );

        assert_eq!(doc["mixed-port"], json!(17897));
        assert_eq!(doc["allow-lan"], json!(true));
        assert_eq!(doc["ipv6"], json!(true));
        assert_eq!(doc["mode"], json!("global"));
        assert_eq!(doc["log-level"], json!("debug"));
        assert!(doc.get("external-controller").is_none());
        assert!(doc.get("external-controller-unix").is_none());
        assert_eq!(
            doc["external-controller-pipe"],
            json!(r"\\.\pipe\verve-mihomo-main")
        );
        assert_eq!(doc["tun"]["enable"], json!(true));

        // Verify UnixSocket branch clears stale pipe field and disables tun.enable when false.
        let mut unix_ctx = base_context();
        unix_ctx.runtime.control_endpoint =
            ControlEndpoint::UnixSocket(PathBuf::from("/run/verve/mihomo.sock"));
        unix_ctx.runtime.tun_enabled = false;

        let unix_compiled = CompilerPipeline::default()
            .compile_to_yaml(
                "external-controller-pipe: stale\ntun:\n  enable: true\n",
                &unix_ctx,
            )
            .unwrap();
        assert!(
            unix_compiled
                .document
                .get("external-controller-pipe")
                .is_none()
        );
        assert_eq!(
            unix_compiled.document["external-controller-unix"],
            json!("/run/verve/mihomo.sock")
        );
        assert_eq!(unix_compiled.document["tun"]["enable"], json!(false));
    }

    struct MockScriptEngine {
        fail_on: Option<&'static str>,
    }

    impl ScriptEngine for MockScriptEngine {
        fn eval_override(
            &self,
            script: &str,
            mut config: Value,
            profile_name: &str,
            _limits: SandboxLimits,
        ) -> Result<ScriptOutput, CompileDiagnostic> {
            if let Some(marker) = self.fail_on
                && script.contains(marker)
            {
                return Err(CompileDiagnostic {
                    stage: String::new(),
                    line: Some(3),
                    column: Some(5),
                    message: "script threw ReferenceError".into(),
                });
            }

            if let Some(map) = config.as_object_mut() {
                map.insert(
                    format!("visited-{script}"),
                    Value::String(profile_name.to_owned()),
                );
            }

            Ok(ScriptOutput {
                config,
                console_logs: vec![format!("executed {script} for {profile_name}")],
            })
        }
    }

    #[test]
    fn custom_script_engine_executes_stages_and_propagates_errors() {
        let engine = Arc::new(MockScriptEngine { fail_on: None });
        let pipeline = CompilerPipeline::new(engine);

        let mut ctx = base_context();
        ctx.profile_script = Some("profile-fn".into());
        ctx.global_script = Some("global-fn".into());

        let compiled = pipeline
            .compile_to_yaml("mixed-port: 7890\n", &ctx)
            .expect("mock script stages must succeed");
        assert_eq!(compiled.document["visited-profile-fn"], json!("work"));
        assert_eq!(compiled.document["visited-global-fn"], json!("work"));
        assert!(
            compiled
                .logs
                .iter()
                .any(|l| l.stage == "profile-script" && l.message.contains("profile-fn"))
        );
        assert!(
            compiled
                .logs
                .iter()
                .any(|l| l.stage == "global-script" && l.message.contains("global-fn"))
        );

        let failing_pipeline = CompilerPipeline::new(Arc::new(MockScriptEngine {
            fail_on: Some("boom"),
        }));
        ctx.global_script = Some("boom()".into());
        let err = failing_pipeline
            .compile_to_yaml("mixed-port: 7890\n", &ctx)
            .expect_err("script error must propagate");
        assert_eq!(err.stage, "global-script");
        assert_eq!(err.line, Some(3));
        assert_eq!(err.column, Some(5));
        assert_eq!(err.message, "script threw ReferenceError");
    }

    #[test]
    fn compile_and_commit_backs_up_active_and_rolls_back_on_preflight_failure() {
        let dir = tempfile::tempdir().expect("tempdir creation must succeed");
        let runtime_dir = dir.path().join("runtime");
        let pipeline = CompilerPipeline::default();

        // 1. First commit creates active.yaml without last_known_good.yaml.
        let mut ctx1 = base_context();
        ctx1.runtime.mixed_port = 7891;
        let committed1 = block_on(pipeline.compile_and_commit(
            "rules:\n  - MATCH,DIRECT\n",
            &ctx1,
            &runtime_dir,
            |candidate_path| {
                let exists = candidate_path.exists();
                async move {
                    assert!(exists, "candidate file must exist during preflight");
                    Ok(())
                }
            },
        ))
        .expect("first commit must succeed");

        assert_eq!(committed1.active_path, runtime_dir.join("active.yaml"));
        assert!(committed1.backup_path.is_none());
        assert!(committed1.active_path.exists());
        assert!(!runtime_dir.join("last_known_good.yaml").exists());
        let active_v1 = std::fs::read_to_string(&committed1.active_path).unwrap();
        assert!(active_v1.contains("7891"));

        // 2. Second commit backs up v1 to last_known_good.yaml and replaces active.yaml.
        let mut ctx2 = base_context();
        ctx2.runtime.mixed_port = 7892;
        let committed2 = block_on(pipeline.compile_and_commit(
            "rules:\n  - MATCH,DIRECT\n",
            &ctx2,
            &runtime_dir,
            |_| async { Ok(()) },
        ))
        .expect("second commit must succeed");

        let backup_path = committed2
            .backup_path
            .as_ref()
            .expect("second commit must create backup");
        assert_eq!(backup_path, &runtime_dir.join("last_known_good.yaml"));
        assert_eq!(std::fs::read_to_string(backup_path).unwrap(), active_v1);
        let active_v2 = std::fs::read_to_string(&committed2.active_path).unwrap();
        assert!(active_v2.contains("7892"));

        // 3. Failing preflight leaves active.yaml & last_known_good.yaml untouched
        //    and removes the candidate temp file.
        let mut ctx3 = base_context();
        ctx3.runtime.mixed_port = 7893;
        let err = block_on(pipeline.compile_and_commit(
            "rules:\n  - MATCH,DIRECT\n",
            &ctx3,
            &runtime_dir,
            |_| async {
                Err(vec![ConfigDiagnostic {
                    line: Some(12),
                    column: Some(4),
                    message: "invalid rule target".into(),
                }])
            },
        ))
        .expect_err("failing preflight must reject commit");

        assert_eq!(err.stage, "preflight");
        assert_eq!(err.line, Some(12));
        assert_eq!(err.column, Some(4));
        assert_eq!(err.message, "invalid rule target");

        assert_eq!(
            std::fs::read_to_string(&committed2.active_path).unwrap(),
            active_v2
        );
        assert_eq!(std::fs::read_to_string(backup_path).unwrap(), active_v1);

        let mut remaining_files: Vec<String> = std::fs::read_dir(&runtime_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        remaining_files.sort();
        assert_eq!(remaining_files, vec!["active.yaml", "last_known_good.yaml"]);
    }
}
