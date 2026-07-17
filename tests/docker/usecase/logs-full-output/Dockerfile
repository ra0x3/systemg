FROM sysg-usecase-base

COPY tests/docker/usecase/logs-full-output/emit.py /usecase/emit.py
COPY tests/docker/usecase/logs-full-output/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/logs-full-output/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
