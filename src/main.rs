use std::sync::RwLock;

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};
use aegis_core::{compiler, loader};
use serde_json::Value;

#[derive(Debug)]
struct Backend {
    client: Client,
    symbols: RwLock<Vec<CompletionItem>>
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    // 1. Initialisation : On dit à VS Code ce qu'on sait faire
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                // On veut être notifié quand le texte change
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                // On supporte l'autocomplétion
                completion_provider: Some(CompletionOptions::default()),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "Aegis LSP initialized!")
            .await;
    }

    // 2. Quand le fichier est ouvert ou modifié : ANALYSE D'ERREURS
    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        self.validate_document(params.text_document.uri, params.text_document.text).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        // En mode FULL sync, content_changes[0].text contient tout le fichier
        if let Some(change) = params.content_changes.into_iter().next() {
            self.validate_document(params.text_document.uri, change.text).await;
        }
    }

    // 3. Autocomplétion
    async fn completion(&self, _: CompletionParams) -> Result<Option<CompletionResponse>> {
        // 1. Liste de base (Mots-clés)
        let keywords = vec![
            "var", "func", "if", "else", "while", "for", "return", 
            "class", "new", "import", "try", "catch", "namespace",
            "true", "false", "null"
        ];

        let mut items: Vec<CompletionItem> = keywords
            .into_iter()
            .map(|k| CompletionItem {
                label: k.to_string(),
                kind: Some(CompletionItemKind::KEYWORD),
                ..Default::default()
            })
            .collect();

        // 2. Ajouter les symboles découverts dynamiquement
        if let Ok(read_guard) = self.symbols.read() {
            // On clone pour renvoyer la liste
            items.extend(read_guard.clone());
        }

        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}

impl Backend {
    // La logique de validation utilise TON compilateur !
    async fn validate_document(&self, uri: Url, text: String) {
        let mut diagnostics = Vec::new();

        match compiler::compile(&text) {
            Ok(json_ast) => {
                // 1. Mise à jour des symboles pour l'autocomplétion
                let found_symbols = self.extract_symbols(&json_ast);
                
                // On met à jour le RwLock
                if let Ok(mut write_guard) = self.symbols.write() {
                    *write_guard = found_symbols;
                }

                // 2. Validation Loader (inchangée)
                if let Err(e) = loader::parse_block(&json_ast) {
                    diagnostics.push(self.parse_error_message(&e));
                }
            },
            Err(e) => {
                diagnostics.push(self.parse_error_message(&e));
            }
        }

        self.client.publish_diagnostics(uri, diagnostics, None).await;
    }

    // Helper pour transformer tes erreurs "[Ligne X] Msg" en format LSP
    fn parse_error_message(&self, msg: &str) -> Diagnostic {
        // Format attendu: "Message d'erreur (Line 10)" ou "[Ligne 10] Message"
        // On essaie d'extraire le numéro de ligne
        let mut line_num = 0;
        
        // Regex simpliste ou parsing manuel.
        // Tes erreurs ressemblent à : "Expect '(' (Line 5)" ou "[Ligne 5] Error"
        
        if let Some(start) = msg.find("(Line ") {
            if let Some(end) = msg[start..].find(')') {
                let num_str = &msg[start + 6 .. start + end];
                if let Ok(n) = num_str.parse::<u32>() {
                    line_num = n.saturating_sub(1); // LSP commence à 0, Aegis à 1
                }
            }
        } else if let Some(start) = msg.find("[Ligne ") {
             if let Some(end) = msg[start..].find(']') {
                let num_str = &msg[start + 7 .. start + end];
                if let Ok(n) = num_str.parse::<u32>() {
                    line_num = n.saturating_sub(1);
                }
            }
        }

        Diagnostic {
            range: Range {
                start: Position { line: line_num, character: 0 },
                end: Position { line: line_num, character: 100 }, // Souligne toute la ligne
            },
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("Aegis Compiler".to_string()),
            message: msg.to_string(),
            ..Default::default()
        }
    }

    fn extract_symbols(&self, ast: &Value) -> Vec<CompletionItem> {
        let mut symbols = Vec::new();

        if let Some(arr) = ast.as_array() {
            // Si le premier élément est une string, c'est une instruction unique
            if !arr.is_empty() && arr[0].is_string() {
                self.analyze_instruction(arr, &mut symbols);
            } else {
                // Sinon c'est une liste d'instructions
                for item in arr {
                    let sub_symbols = self.extract_symbols(item);
                    symbols.extend(sub_symbols);
                }
            }
        }

        symbols
    }

    fn analyze_instruction(&self, arr: &Vec<Value>, symbols: &mut Vec<CompletionItem>) {
        if arr.is_empty() { return; }
        
        // CORRECTION 1 : as_str() sur serde_json renvoie Option<&str>
        let cmd = arr[0].as_str().unwrap_or(""); 
        
        match cmd {
            "set" => {
                // ["set", line, "nom_var", ...]
                // CORRECTION 2 : on utilise get(2) car index 1 est la ligne
                if let Some(name) = arr.get(2).and_then(|v| v.as_str()) {
                    symbols.push(CompletionItem {
                        label: name.to_string(),
                        kind: Some(CompletionItemKind::VARIABLE),
                        detail: Some("Variable".to_string()),
                        ..Default::default()
                    });
                }
            },
            "function" => {
                // ["function", line, "nom_func", params, ret, body]
                if let Some(name) = arr.get(2).and_then(|v| v.as_str()) {
                    symbols.push(CompletionItem {
                        label: name.to_string(),
                        kind: Some(CompletionItemKind::FUNCTION),
                        detail: Some("Function".to_string()),
                        insert_text: Some(format!("{}($0)", name)),
                        insert_text_format: Some(InsertTextFormat::SNIPPET),
                        ..Default::default()
                    });
                }
                // Récursion body (index 5)
                if let Some(body) = arr.get(5) {
                    self.extract_symbols(body).into_iter().for_each(|s| symbols.push(s));
                }
            },
            "class" => {
                // ["class", line, "Name", ...]
                if let Some(name) = arr.get(2).and_then(|v| v.as_str()) {
                    symbols.push(CompletionItem {
                        label: name.to_string(),
                        kind: Some(CompletionItemKind::CLASS),
                        detail: Some("Class".to_string()),
                        ..Default::default()
                    });
                }
            },
            "namespace" => {
                // ["namespace", line, "Name", body]
                if let Some(name) = arr.get(2).and_then(|v| v.as_str()) {
                    symbols.push(CompletionItem {
                        label: name.to_string(),
                        kind: Some(CompletionItemKind::MODULE),
                        detail: Some("Namespace".to_string()),
                        ..Default::default()
                    });
                }
                // Récursion body (index 3)
                if let Some(body) = arr.get(3) {
                    self.extract_symbols(body).into_iter().for_each(|s| symbols.push(s));
                }
            },
            
            // Blocs récursifs
            "if" | "while" | "for_range" => {
                // On scanne les arguments à partir de l'index 2
                for arg in &arr[2..] {
                    self.extract_symbols(arg).into_iter().for_each(|s| symbols.push(s));
                }
            },
            
            _ => {}
        }
    }
}

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(|client| Backend { 
        client,
        symbols: RwLock::new(Vec::new())
    });
    Server::new(stdin, stdout, socket).serve(service).await;
}
