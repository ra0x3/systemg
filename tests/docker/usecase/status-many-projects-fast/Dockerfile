FROM sysg-usecase-base

COPY tests/docker/usecase/status-many-projects-fast/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
