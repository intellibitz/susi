import re
admin_path = 'src/daemon/admin.rs'
with open(admin_path, 'r') as f:
    code = f.read()

helper_funcs = """    pub fn get_global_dir() -> std::path::PathBuf {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        home.join(".susi")
    }

    pub fn get_cargo_version(workspace: &std::path::Path) -> EaiResult<String> {
        let cargo_toml_path = workspace.join("Cargo.toml");
        let content = std::fs::read_to_string(&cargo_toml_path)?;
        content
            .lines()
            .find(|l| l.trim().starts_with("version = \\\""))
            .and_then(|l| l.split('"').nth(1))
            .map(|s| s.to_string())
            .ok_or_else(|| EaiError::config("Could not find version in Cargo.toml"))
    }"""

old_home1 = """            let home = std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            let global_dir = home.join(".susi");"""

old_home2 = """        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("."));"""


old_version = """        let cargo_toml_path = workspace.join("Cargo.toml");
        let content = fs::read_to_string(&cargo_toml_path)?;

        let version = content
            .lines()
            .find(|l| l.trim().starts_with("version = \\\""))
            .and_then(|l| l.split('"').nth(1))
            .ok_or_else(|| EaiError::config("Could not find version in Cargo.toml"))?;"""

# Let's write a smarter regex substitution
# Replace version logic
code = re.sub(
    r'let cargo_toml_path.*?EaiError::config\("Could not find version in Cargo\.toml"\)\)\?;',
    'let version_str = Self::get_cargo_version(workspace)?;\n        let version = version_str.as_str();',
    code,
    flags=re.DOTALL
)

# Replace home logic (global_dir) - exact matches: Let's do exact block replaces instead of regex to avoid breaks.
