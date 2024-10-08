use actix_web::{get, web, App, HttpServer, Responder, HttpResponse, http::header::ContentType};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use serde_json::Value;
use actix_cors::Cors;
use chrono::Utc;
use std::sync::Mutex;
use std::env;
use dotenv::dotenv;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;




#[derive(Deserialize, Debug, Clone)]
struct CoinData {
    name: String,
    symbol: String,
    quote: HashMap<String, QuoteData>,
}

#[derive(Deserialize, Debug, Clone)]
struct QuoteData {
    percent_change_24h: f64,
    percent_change_7d: f64,
}

#[derive(Deserialize, Debug)]
struct ApiResponse {
    data: Vec<CoinData>,
}

#[derive(Serialize, Deserialize)]
struct FinalSummary {
    token: String,
    symbol: String,
    summary: String,
    references: Vec<String>,
}

// Shared state to store summaries in memory
struct AppState {
    summaries: Mutex<HashMap<String, Vec<FinalSummary>>>,
}

async fn save_summaries_to_file(data: &Vec<FinalSummary>, date_str: &str) -> std::io::Result<()> {
    let dir_path = Path::new("data/");
    if !dir_path.exists() {
        fs::create_dir_all(dir_path)?;
    }
    
    let file_path = dir_path.join(format!("{}.json", date_str));
    let mut file = File::create(file_path)?;
    let json_data = serde_json::to_string_pretty(data)?;
    file.write_all(json_data.as_bytes())?;
    Ok(())
}

async fn load_summaries_from_file(date_str: &str) -> std::io::Result<Vec<FinalSummary>> {
    let file_path = Path::new("data/").join(format!("{}.json", date_str));
    let mut file = File::open(file_path)?;
    let mut content = String::new();
    file.read_to_string(&mut content)?;
    let data: Vec<FinalSummary> = serde_json::from_str(&content)?;
    Ok(data)
}

async fn format_crypto_data(
    data: &Vec<CoinData>,
    news_api_key: &str,
    groq_api_key: &str,
    groq_api_base_url: &str
) -> Vec<FinalSummary> {
    let mut growth_coins = data.clone();
    let mut decline_coins = data.clone();

    growth_coins.sort_by(|a, b| b.quote["USD"].percent_change_24h.partial_cmp(&a.quote["USD"].percent_change_24h).unwrap());
    decline_coins.sort_by(|a, b| a.quote["USD"].percent_change_24h.partial_cmp(&b.quote["USD"].percent_change_24h).unwrap());

    let mut summaries = Vec::new();

    let combined_coins = growth_coins.iter().take(3).chain(decline_coins.iter().take(3));
    
    for coin in combined_coins {
        let query = format!("{} cryptocurrency", coin.name);
        let news = fetch_news(news_api_key, &query).await.unwrap_or_else(|_| vec!["No news found".to_string()]);
        
        let summary_prompt = format!(
            "Analyze the following data:\n\nCoin: {}\nSymbol: {}\n24h Change: {:.2}%\n7d Change: {:.2}%\nRecent News:\n{}\n",
            coin.name,
            coin.symbol,
            coin.quote["USD"].percent_change_24h,
            coin.quote["USD"].percent_change_7d,
            news.join("\n")
        );
        
        let summary = call_groq_api(groq_api_key, groq_api_base_url, "mixtral-8x7b-32768", &summary_prompt, 320).await.unwrap_or_else(|e| {
            println!("Error fetching summary: {}", e);
            "No summary available.".to_string()
        });

        summaries.push(FinalSummary {
            token: coin.name.clone(),
            symbol: coin.symbol.clone(),
            summary,
            references: news,
        });

        tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
    }

    summaries
}

async fn call_groq_api(api_key: &str, base_url: &str, model: &str, prompt: &str, max_tokens: u32) -> Result<String, Box<dyn std::error::Error>> {
    print!("Calling GROQ API with prompt: {}", prompt);
    let url = format!("{}/openai/v1/chat/completions", base_url.trim_end_matches('/'));
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {}", api_key))?);
    let payload = serde_json::json!({
        "model": model,
        "messages": [
            {
                "role": "system",
                "content": "Response must not exceed 280 tokens. You are a crypto analyst and journalist. You analyze news/prices and summarize the data. The summary for each token must use the following template: 'Analyze the following data: Coin: {coin_name}, Symbol: {coin_symbol}, 24h Change: {24h_change}%, 7d Change: {7d_change}%, What caused: {news analysis}'. The summary must not exceed 300 tokens and must NOT include any URLs, references, or links. Just provide the analysis in a newspaper style. If the one of news is not related to the token - just ignore it. Dont eveven mention it. You don't have to list the news. We need a solid, stylish and beautiful newspaper paragraph that highlights the most important news and describes them to readers in the context of the reason for the rise or fall in the price of the token. Make sure that all sentences are logically connected and full."
            },
            {
                "role": "user",
                "content": prompt
            }
        ],
        "temperature": 0.5,
        "max_tokens": max_tokens,
        "top_p": 1.0,
        "stop": null,
        "stream": false
    });
    let client = reqwest::Client::new();
    let res = client.post(&url).headers(headers).json(&payload).send().await?;
    let body = res.text().await?;
    let json: Value = serde_json::from_str(&body)?;
    if let Some(choices) = json["choices"].as_array() {
        if let Some(choice) = choices.get(0) {
            if let Some(message) = choice["message"].as_object() {
                if let Some(content) = message.get("content") {
                    return Ok(content.as_str().unwrap_or("").to_string());
                }
            }
        }
    }

    println!("Unexpected response structure: {:?}", json);
    Err("No valid response received.".into())
    
}

async fn fetch_news(api_key: &str, query: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    println!("Fetching news for query: {}", query);

    let url = format!(
        "https://newsapi.org/v2/everything?q={}&sortBy=publishedAt&apiKey={}",
        query, api_key
    );

    let client = reqwest::Client::new();
    let res = client.get(&url).header(USER_AGENT, "Rust News Client").send().await?;
    let body = res.text().await?;

    let json: Value = match serde_json::from_str(&body) {
        Ok(json) => json,
        Err(e) => {
            println!("Failed to parse JSON: {}", e);
            return Err(Box::new(e));
        }
    };

    let mut news = Vec::new();
    if let Some(articles) = json["articles"].as_array() {
        for article in articles.iter().take(12) {
            if let Some(title) = article["title"].as_str() {
                if let Some(url) = article["url"].as_str() {
                    news.push(format!("{} - {}", title, url));
                }
            }
        }
    } else {
        println!("No articles found in response.");
    }

    Ok(news)
}


async fn fetch_and_store_summaries(state: web::Data<AppState>) -> Result<(), Box<dyn std::error::Error>> {
    dotenv().ok();
    let coinmarketcap_api_key = env::var("COINMARKETCAP_API_KEY").expect("COINMARKETCAP_API_KEY not set");
    let groq_api_key = env::var("GROQ_API_KEY").expect("GROQ_API_KEY not set");
    let news_api_key = env::var("NEWS_API_KEY").expect("NEWS_API_KEY not set");
    let groq_api_base_url = env::var("GROQ_API_BASE_URL").expect("GROQ_API_BASE_URL not set");

    let coinmarketcap_url = "https://pro-api.coinmarketcap.com/v1/cryptocurrency/listings/latest";

    let mut headers = HeaderMap::new();
    headers.insert("X-CMC_PRO_API_KEY", HeaderValue::from_str(coinmarketcap_api_key.as_str()).unwrap());
    headers.insert(USER_AGENT, HeaderValue::from_static("Rust Client"));

    let client = reqwest::Client::new();
    let res = client.get(coinmarketcap_url).headers(headers).send().await;

    if let Ok(res) = res {
        let status = res.status();
        let body = res.text().await.unwrap_or_else(|_| "Failed to read response body".to_string());

        if status.is_success() {
            match serde_json::from_str::<ApiResponse>(&body) {
                Ok(api_response) => {
                    let summaries = format_crypto_data(&api_response.data, news_api_key.as_str(), groq_api_key.as_str(), groq_api_base_url.as_str()).await;
                    let date_str = Utc::now().format("%Y-%m-%d").to_string();

                    // Save summaries to file
                    save_summaries_to_file(&summaries, &date_str).await?;

                    // Optionally store in memory (not necessary if using files)
                    let mut summaries_map = state.summaries.lock().unwrap();
                    summaries_map.insert(date_str.clone(), summaries);

                }
                Err(e) => {
                    println!("Failed to parse API response: {}", e);
                }
            }
        } else {
            println!("CoinMarketCap API returned error: {}", body);
        }
    } else {
        println!("Failed to fetch data from CoinMarketCap API");
    }

    Ok(())
}

#[get("/crypto-summary")]
async fn get_crypto_summary(state: web::Data<AppState>) -> impl Responder {
    let date_str = Utc::now().format("%Y-%m-%d").to_string();

    // Try to load summaries from file first
    if let Ok(summaries) = load_summaries_from_file(&date_str).await {
        HttpResponse::Ok()
            .content_type(ContentType::json())
            .json(summaries)
    } else {
        // If not found, fetch and store new summaries
        if let Err(e) = fetch_and_store_summaries(state.clone()).await {
            return HttpResponse::InternalServerError().body(format!("Failed to fetch and store summaries: {}", e));
        }
        let summaries_map = state.summaries.lock().unwrap();
        if let Some(summaries) = summaries_map.get(&date_str) {
            HttpResponse::Ok()
                .content_type(ContentType::json())
                .json(summaries)
        } else {
            HttpResponse::InternalServerError().body("Failed to generate summaries")
        }
    }
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let app_state = web::Data::new(AppState {
        summaries: Mutex::new(HashMap::new()),
    });

    HttpServer::new(move || {
        let cors = Cors::default()
            .allow_any_origin()
            .allow_any_method()
            .allow_any_header();

        App::new()
            .app_data(app_state.clone())
            .wrap(cors)
            .service(get_crypto_summary)
    })
    .bind(("0.0.0.0", 8066))? 
    .run()
    .await
}