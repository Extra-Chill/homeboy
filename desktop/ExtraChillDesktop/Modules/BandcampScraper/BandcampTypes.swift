import Foundation

struct ScraperResult: Codable {
    let success: Bool
    let tag: String
    let totalAlbumsScraped: Int
    let results: [ScrapedEmail]
    let errors: [String]
    
    enum CodingKeys: String, CodingKey {
        case success
        case tag
        case totalAlbumsScraped = "total_albums_scraped"
        case results
        case errors
    }
}

struct ScrapedEmail: Codable, Identifiable, Hashable {
    var id: String { email }
    let email: String
    let name: String
    let notes: String
    let sourceUrl: String
    
    enum CodingKeys: String, CodingKey {
        case email
        case name
        case notes
        case sourceUrl = "source_url"
    }
}
