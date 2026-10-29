fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        let xml = std::fs::read_to_string(&args[1]).unwrap();
        match asclepius_core::literature::parse_pubmed_xml(&xml) {
            articles if !articles.is_empty() => {
                println!("file articles: {}", articles.len());
                for a in articles.iter().take(3) {
                    println!(
                        "  {} | {}",
                        a.pmid,
                        a.title.chars().take(60).collect::<String>()
                    );
                }
            }
            _ => println!("file articles: 0 (parser returned empty)"),
        }
    } else {
        let client = asclepius_core::literature::PubMedClient::from_env();
        match client.search_pmids("zinc acne", 3) {
            Ok(pmids) => {
                println!("esearch: {pmids:?}");
                match client.fetch_articles(&pmids) {
                    Ok(articles) => {
                        println!("fetched: {}", articles.len());
                        for a in articles.iter().take(3) {
                            println!(
                                "  {} | {} | abstract {} chars",
                                a.pmid,
                                a.title.chars().take(60).collect::<String>(),
                                a.abstract_text.len()
                            );
                        }
                    }
                    Err(e) => println!("fetch error: {e}"),
                }
            }
            Err(e) => println!("esearch error: {e}"),
        }
    }
}
