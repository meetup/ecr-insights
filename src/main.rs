use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use rusoto_core::Region;
use rusoto_ecr::{
    DescribeImagesRequest, DescribeRepositoriesRequest, Ecr, EcrClient, ImageDetail, Repository,
};
use std::{
    error::Error,
    io::{stdout, Error as IoError, Write},
};
use structopt::StructOpt;
use tabwriter::TabWriter;

struct Repo {
    name: String,
    last_pushed_at: Option<String>,
    latest_image_size: i64,
    aggregate_image_size: i64,
    hosted_images: usize,
}

impl Repo {
    /// Storage is $0.10 per GB-month
    /// https://aws.amazon.com/ecr/pricing/
    fn monthly_cost(&self) -> f64 {
        (self.aggregate_image_size as f64 / (1024 * 1024 * 1024) as f64) * 0.10
    }
}

#[derive(StructOpt)]
struct Opts {
    #[structopt(long, short, default_value = "tsv")]
    /// output format: tsv or csv
    format: String,
}

fn load_all_images(
    ecr: &EcrClient,
    repository_name: String,
    next: Option<String>,
) -> Result<Vec<ImageDetail>, Box<dyn Error>> {
    let result = ecr
        .describe_images(DescribeImagesRequest {
            repository_name: repository_name.clone(),
            max_results: Some(1_000),
            next_token: next,
            ..DescribeImagesRequest::default()
        })
        .sync()?;
    if result.next_token.is_some() {
        let mut images = result.image_details.unwrap_or_default();
        images.append(&mut load_all_images(
            ecr,
            repository_name,
            result.next_token,
        )?);
        Ok(images)
    } else {
        Ok(result.image_details.unwrap_or_default())
    }
}

fn load_all_repositories(
    ecr: &EcrClient,
    next: Option<String>,
) -> Result<Vec<Repository>, Box<dyn Error>> {
    let result = ecr
        .describe_repositories(DescribeRepositoriesRequest {
            max_results: Some(1_000),
            next_token: next,
            ..DescribeRepositoriesRequest::default()
        })
        .sync()?;
    if result.next_token.is_some() {
        let mut repositories = result.repositories.unwrap_or_default();
        repositories.append(&mut load_all_repositories(ecr, result.next_token)?);
        Ok(repositories)
    } else {
        Ok(result.repositories.unwrap_or_default())
    }
}

fn pushed_at(details: &ImageDetail) -> NaiveDateTime {
    NaiveDateTime::from_timestamp(details.image_pushed_at.unwrap_or_default() as i64, 0)
}

fn repos(ecr: &EcrClient) -> Result<Vec<Repo>, Box<dyn Error>> {
    load_all_repositories(&ecr, None)?
        .into_iter()
        .try_fold(Vec::new(), |mut repos, repo| {
            let repository_name = repo.repository_name.unwrap_or_default();
            let mut images = load_all_images(&ecr, repository_name.clone(), None)?;
            let now = Utc::now().naive_utc();
            let first_of_the_month = NaiveDateTime::new(
                NaiveDate::from_ymd(now.year(), now.month(), 1),
                NaiveTime::from_hms(0, 0, 0),
            );
            images.retain(|details| pushed_at(details) < first_of_the_month);
            images.sort_by(|a, b| pushed_at(b).cmp(&pushed_at(a)));
            repos.push(Repo {
                name: repository_name,
                last_pushed_at: images
                .iter()
                .next()
                .map(|details| pushed_at(details).to_string()),
                latest_image_size: images
                    .iter()
                    .next()
                    .map(|details| details.image_size_in_bytes.unwrap_or_default())
                    .unwrap_or_default(),
                aggregate_image_size: images
                    .iter()
                    .map(|details| details.image_size_in_bytes.unwrap_or_default())
                    .sum(),
                hosted_images: images.len(),
            });
            Ok(repos)
        })
}

fn main() -> Result<(), Box<dyn Error>> {
    let Opts { format } = Opts::from_args();
    let ecr = EcrClient::new(Region::default());
    let mut writer = TabWriter::new(stdout());
    let mut repos = repos(&ecr)?;
    repos.sort_by(|a, b| b.latest_image_size.cmp(&a.latest_image_size));
    let total_cost: Result<f64, IoError> = repos.into_iter().try_fold(0f64, |cost, repo| {
        let monthly_cost = repo.monthly_cost();
        let Repo {
            name,
            last_pushed_at,
            latest_image_size,
            hosted_images,
            ..
        } = repo;
        match &format[..] {
            "tsv" => {
                writeln!(
                    writer,
                    "{}\t{}\t{}\t{}\t${:.2}",
                    name, last_pushed_at.unwrap_or_default(), latest_image_size, hosted_images, monthly_cost
                )?;
            }
            "csv" => {
                println!(
                    "{},{}, {},{},${:.2}",
                    name, last_pushed_at.unwrap_or_default(), latest_image_size, hosted_images, monthly_cost
                );
            }
            _ => (),
        }

        Ok(cost + monthly_cost)
    });
    match &format[..] {
        "tsv" => {
            writeln!(writer, "\t\t\t\t${:.2}", total_cost?)?;
            writer.flush()?;
        }
        _ => (),
    }

    Ok(())
}
