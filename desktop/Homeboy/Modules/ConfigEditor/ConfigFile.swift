import Foundation

/// Represents the three editable server configuration files
enum ConfigFile: String, CaseIterable, Identifiable {
    case wpConfig = "wp-config.php"
    case htaccess = ".htaccess"
    case robotsTxt = "robots.txt"
    
    var id: String { rawValue }
    
    var displayName: String { rawValue }
    
    var icon: String {
        switch self {
        case .wpConfig: return "gearshape.fill"
        case .htaccess: return "lock.shield"
        case .robotsTxt: return "ant"
        }
    }
    
    /// Path relative to appPath
    func remotePath(appPath: String) -> String {
        "\(appPath)/\(rawValue)"
    }
    
    /// Default template for creating new files
    var defaultTemplate: String {
        switch self {
        case .wpConfig:
            // wp-config should always exist, no creation template
            return ""
        case .htaccess:
            return """
            # BEGIN WordPress
            <IfModule mod_rewrite.c>
            RewriteEngine On
            RewriteBase /
            RewriteRule ^index\\.php$ - [L]
            RewriteCond %{REQUEST_FILENAME} !-f
            RewriteCond %{REQUEST_FILENAME} !-d
            RewriteRule . /index.php [L]
            </IfModule>
            # END WordPress
            """
        case .robotsTxt:
            let domain = ConfigurationManager.readCurrentProject().domain
            let sitemapUrl = domain.isEmpty ? "https://example.com/sitemap.xml" : "https://\(domain)/sitemap.xml"
            return """
            User-agent: *
            Disallow: /wp-admin/
            Allow: /wp-admin/admin-ajax.php
            
            Sitemap: \(sitemapUrl)
            """
        }
    }
    
    /// Whether this file can be created if missing
    var canCreate: Bool {
        switch self {
        case .wpConfig: return false // Should always exist
        case .htaccess, .robotsTxt: return true
        }
    }
    
    /// Warning message shown in save confirmation
    var saveWarning: String {
        switch self {
        case .wpConfig:
            return "wp-config.php contains critical database and security settings. An error could break your entire site."
        case .htaccess:
            return ".htaccess controls URL routing and access rules. An error could make your site inaccessible."
        case .robotsTxt:
            return "robots.txt controls search engine indexing. Incorrect rules could harm your SEO."
        }
    }
}
